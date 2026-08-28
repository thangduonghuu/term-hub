import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import { MessageLog } from "./components/MessageLog";

// The same bundle backs two webviews (see `lib.rs`'s `build_panel`): the main window and the
// right-docked message-log panel, told apart by `?panel=messages`.
const panel = new URLSearchParams(window.location.search).get("panel");

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>{panel === "messages" ? <MessageLog /> : <App />}</React.StrictMode>,
);
