type Answer = unknown | (() => unknown);

function noop(): void {}

export class FakeBridge {
  private readonly answers = new Map<string, Answer>();
  private readonly gates = new Map<string, (() => Promise<void>)[]>();
  private readonly eventHandlers = new Map<string, Set<(payload: unknown) => void>>();
  readonly calls: { method: string; params: object | undefined }[] = [];

  answer(method: string, value: Answer): void {
    this.answers.set(method, value);
  }

  /// Gates the *next* call to `method` until the returned function is invoked.
  hold(method: string): () => void {
    let release = noop;
    const gate = new Promise<void>((resolve) => {
      release = resolve;
    });
    const queue = this.gates.get(method) ?? [];
    queue.push(() => gate);
    this.gates.set(method, queue);
    return release;
  }

  call = <T>(method: string, params?: object): Promise<T> => {
    this.calls.push({ method, params });
    const gate = this.gates.get(method)?.shift();
    const wait = gate === undefined ? Promise.resolve() : gate();
    return wait.then(() => {
      const answer = this.answers.get(method);
      if (typeof answer === "function") return (answer as () => T)();
      return answer as T;
    });
  };

  on = (name: string, handler: (payload: unknown) => void): (() => void) => {
    let set = this.eventHandlers.get(name);
    if (set === undefined) {
      set = new Set();
      this.eventHandlers.set(name, set);
    }
    set.add(handler);
    return () => {
      set.delete(handler);
    };
  };

  emit(name: string, payload: unknown): void {
    const set = this.eventHandlers.get(name);
    if (set === undefined) return;
    for (const handler of set) handler(payload);
  }
}
