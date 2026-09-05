import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { App } from "./App";
import "./lib/bridge";
import "./styles/globals.css";

const root = document.getElementById("root");
if (root === null) throw new Error("no #root in the page");

createRoot(root).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
