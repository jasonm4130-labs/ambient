import { createContext, useContext, type ReactNode } from "react";
import type { Bridge } from "./bridge";

/// No default value: a silent fallback to the real bridge would let a
/// component under test post through `window.webkit`, which does not exist
/// in jsdom, so the call would hang instead of failing with a message.
/// `useBridge` throws instead of returning one.
const BridgeContext = createContext<Bridge | undefined>(undefined);

export function BridgeProvider({
  bridge,
  children,
}: {
  bridge: Bridge;
  children: ReactNode;
}) {
  return <BridgeContext.Provider value={bridge}>{children}</BridgeContext.Provider>;
}

export function useBridge(): Bridge {
  const found = useContext(BridgeContext);
  if (found === undefined) {
    throw new Error("useBridge called with no BridgeProvider above it");
  }
  return found;
}
