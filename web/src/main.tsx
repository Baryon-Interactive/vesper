import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import App from "./App";
import "./styles.css";

// The store origin never runs a service worker: games and their worker live on
// the separate play origin. Remove any registration left by older builds.
navigator.serviceWorker?.getRegistrations().then((registrations) => registrations.forEach((r) => r.unregister()));

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
