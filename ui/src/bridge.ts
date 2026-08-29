import { Context, Effect, Layer } from "effect";
import type { Patch } from "./settings";

/// The seam between the page and Rust. A service rather than a bare function
/// so the wiring can be exercised without a WKWebView around it — in a plain
/// browser `window.webkit` is absent, and a page that throws there would be
/// untestable and undebuggable.
export class Bridge extends Context.Tag("Bridge")<
  Bridge,
  { readonly send: (patch: Patch) => Effect.Effect<void> }
>() {}

interface WebKitWindow {
  webkit?: {
    messageHandlers?: {
      ambient?: { postMessage: (body: string) => void };
    };
  };
}

/// Posts to the `ambient` script-message handler registered in
/// `src/settings.rs`. The body is a JSON *string*: `WKScriptMessage::body`
/// otherwise arrives as an `NSDictionary` that Rust would have to unpick a
/// value at a time.
export const BridgeLive = Layer.succeed(Bridge, {
  send: (patch: Patch) =>
    Effect.sync(() => {
      const handler = (window as unknown as WebKitWindow).webkit?.messageHandlers?.ambient;
      if (handler === undefined) {
        console.warn("no Rust bridge on this page; dropped", patch);
        return;
      }
      // Not `window.postMessage`: this is WKScriptMessageHandler's, which
      // takes a body and no target origin.
      // oxlint-disable-next-line unicorn/require-post-message-target-origin
      handler.postMessage(JSON.stringify(patch));
    }),
});
