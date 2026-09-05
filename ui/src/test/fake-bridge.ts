/// A stand-in for `ui/src/lib/bridge.ts` used by component tests in units 3–6.
/// Answers `call(method, params)` from a method→value map, and can hold a
/// reply until the test releases it — so a test can render a component,
/// select something else, and only then let the first request's answer land,
/// to check that the later selection won it.
///
/// `bridge.test.ts` does not use this: it exercises the real wire by spying on
/// `window.webkit.messageHandlers.ambient` and driving `window.ambient.reply`
/// directly.
type Answer = unknown | (() => unknown);

function noop(): void {}

export class FakeBridge {
  private readonly answers = new Map<string, Answer>();
  private readonly gates = new Map<string, (() => Promise<void>)[]>();
  private readonly eventHandlers = new Map<string, Set<(payload: unknown) => void>>();
  readonly calls: { method: string; params: object | undefined }[] = [];

  /// Sets the value (or a thunk producing it) that `call(method, …)` resolves
  /// to.
  answer(method: string, value: Answer): void {
    this.answers.set(method, value);
  }

  /// The *next* call to `method` resolves only once the returned function is
  /// invoked.
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

  /// Fires `event(name, payload)` to every registered handler, as
  /// `window.ambient.event` would.
  emit(name: string, payload: unknown): void {
    const set = this.eventHandlers.get(name);
    if (set === undefined) return;
    for (const handler of set) handler(payload);
  }
}
