import React from "react";
import { createRoot } from "react-dom/client";
import App from "./App";
import { Context } from "./ui";
import { createController } from "./controller";
import "uplot/dist/uPlot.min.css";
import "./styles.css";
import { initializeTheme } from "./theme";

if (typeof document !== "undefined")
  initializeTheme(document, () => window.localStorage);
export function mountApp(
  doc: Document,
  win: import("./cache").BrowserWindow,
  options: import("./controller").ControllerOptions = {},
) {
  const app = createController(win, options),
    root = createRoot(doc.getElementById("root")!);
  root.render(
    <Context.Provider value={app}>
      <App />
    </Context.Provider>,
  );
  app.start();
  return {
    ...app,
    unmount: () => {
      app.stop();
      root.unmount();
    },
  };
}
if (typeof document !== "undefined" && document.getElementById("root"))
  mountApp(document, window);
