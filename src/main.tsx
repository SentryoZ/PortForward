import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import { tauriBridge } from "./lib/tauriBridge";
import "./styles.css";

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <App bridge={tauriBridge} />
  </React.StrictMode>,
);
