/// The seam between the page and Rust. Request/response over the WKWebView
/// script-message channel: the page posts `{id, method, params}` as a JSON
/// string, and Rust answers asynchronously by calling
/// `window.ambient.reply(id, {result} | {error: {kind, message}})` through
/// `evaluateJavaScript`. `window.ambient.event(name, payload)` is the other
/// direction, one-way, for `config`, `navigate`, `phase` and `diarize`.
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

/// Posts `{id, method, params}` to the `ambient` script-message handler
/// registered in `src/settings.rs` and resolves when Rust answers by id. The
/// body is a JSON *string*: `WKScriptMessage::body` otherwise arrives as an
/// `NSDictionary` that Rust would have to unpick a value at a time.
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
    // Not `window.postMessage`: this is WKScriptMessageHandler's, which takes
    // a body and no target origin.
    // oxlint-disable-next-line unicorn/require-post-message-target-origin
    handler.postMessage(JSON.stringify({ id, method, params }));
  });
}

/// Registers `handler` for `window.ambient.event(name, payload)`. Returns an
/// unsubscribe function.
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

// Installed from the page so Rust can call `window.ambient.reply(id, …)` and
// `window.ambient.event(name, payload)` through `evaluateJavaScript`.
window.ambient = { reply, event };

/// The seam a screen actually depends on, so a test can hand it a
/// `FakeBridge` instead of the real wire.
export interface Bridge {
  call<T>(method: string, params?: object): Promise<T>;
  on(name: string, handler: EventHandler): () => void;
}

/// The stable singleton wrapping the module functions above — same wire
/// behaviour and `window.ambient` installation, just named so a provider can
/// hand it out as a `Bridge`.
export const bridge: Bridge = { call, on };
