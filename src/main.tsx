import React from "react";
import ReactDOM from "react-dom/client";
import { invoke } from "@tauri-apps/api/core";
import App from "./App";

// A JS exception inside a webview is otherwise invisible: no console, no
// crash, just a blank window. Forward everything to the Rust log.
const report = (level: string, message: string) => {
  void invoke("log_frontend", { level, message }).catch(() => {});
};

window.addEventListener("error", (e) =>
  report("error", `${e.message} @ ${e.filename}:${e.lineno}:${e.colno}`),
);
window.addEventListener("unhandledrejection", (e) =>
  report("error", `unhandled rejection: ${String(e.reason)}`),
);

const root = document.getElementById("root");
if (!root) {
  report("error", "#root element missing");
} else {
  try {
    ReactDOM.createRoot(root).render(
      <React.StrictMode>
        <App />
      </React.StrictMode>,
    );
    report("info", "frontend mounted");
  } catch (err) {
    report("error", `render failed: ${String(err)}`);
  }
}
