import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { App } from "./App";
import { startActivity } from "./activity";
import { startEvents } from "./hub";
import "./styles.css";

startEvents();
startActivity();

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
