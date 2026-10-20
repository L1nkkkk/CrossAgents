import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { App } from "./App";
import { startEvents } from "./hub";
import "./styles.css";

startEvents();

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
