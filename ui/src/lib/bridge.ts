interface WebKitWindow {
  webkit?: {
    messageHandlers?: {
      ambient?: { postMessage: (body: string) => void };
    };
  };
}

interface ReplyError {
  kind: string;
  message: string;
}

interface ReplyBody {
  result?: unknown;
  error?: ReplyError;
}

type EventHandler = (payload: unknown) => void;

interface PendingCall {
  resolve: (value: unknown) => void;
  reject: (reason: Error) => void;
}

const pending = new Map<number, PendingCall>();
const handlers = new Map<string, Set<EventHandler>>();
let nextId = 1;

/// The body is a JSON string: otherwise `WKScriptMessage::body` arrives as an
/// `NSDictionary`.
export function call<T>(method: string, params?: object): Promise<T> {
  const id = nextId;
  nextId += 1;
  return new Promise<T>((resolve, reject) => {
    pending.set(id, { resolve: resolve as (value: unknown) => void, reject });

    const handler = (window as unknown as WebKitWindow).webkit?.messageHandlers?.ambient;
    if (handler === undefined) {
      console.warn("no Rust bridge on this page; dropped", method, params);
      return;
    }
    // WKScriptMessageHandler's `postMessage` takes no target origin.
    // oxlint-disable-next-line unicorn/require-post-message-target-origin
    handler.postMessage(JSON.stringify({ id, method, params }));
  });
}

export function on(name: string, handler: EventHandler): () => void {
  let set = handlers.get(name);
  if (set === undefined) {
    set = new Set();
    handlers.set(name, set);
  }
  set.add(handler);
  return () => {
    set.delete(handler);
  };
}

function reply(id: number, body: ReplyBody): void {
  const found = pending.get(id);
  if (found === undefined) return;
  pending.delete(id);
  if (body.error === undefined) {
    found.resolve(body.result);
  } else {
    found.reject(new Error(body.error.message));
  }
}

function event(name: string, payload: unknown): void {
  const set = handlers.get(name);
  if (set === undefined) return;
  for (const handler of set) handler(payload);
}

declare global {
  interface Window {
    ambient: {
      reply: typeof reply;
      event: typeof event;
    };
  }
}

window.ambient = { reply, event };

export interface Bridge {
  call<T>(method: string, params?: object): Promise<T>;
  on(name: string, handler: EventHandler): () => void;
}

export const bridge: Bridge = { call, on };
