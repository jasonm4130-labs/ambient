import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { App } from "./App";
import { bridge } from "./lib/bridge";
import { BridgeProvider } from "./lib/bridge-context";
import "./styles/globals.css";

const root = document.getElementById("root");
if (root === null) throw new Error("no #root in the page");

createRoot(root).render(
  <StrictMode>
    <BridgeProvider bridge={bridge}>
      <App />
    </BridgeProvider>
  </StrictMode>,
);
