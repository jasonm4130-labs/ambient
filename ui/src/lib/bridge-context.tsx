import { createContext, useContext, type ReactNode } from "react";
import type { Bridge } from "./bridge";

const BridgeContext = createContext<Bridge | undefined>(undefined);

export function BridgeProvider({ bridge, children }: { bridge: Bridge; children: ReactNode }) {
  return <BridgeContext.Provider value={bridge}>{children}</BridgeContext.Provider>;
}

export function useBridge(): Bridge {
  const found = useContext(BridgeContext);
  if (found === undefined) {
    throw new Error("useBridge called with no BridgeProvider above it");
  }
  return found;
}
