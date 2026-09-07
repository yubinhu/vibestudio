import { createRoot } from "react-dom/client";
import Comparison from "./Comparison";
import "@fontsource-variable/inter";
import "@/globals.css";

const view = new URLSearchParams(window.location.search).get("view");
if (view === "current" || view === "proposed") {
  // Load the real app only inside the preview. The comparison toolbar does not
  // subscribe to sessions, discover skills, or initialize any backend services.
  void import("./frame");
} else {
  createRoot(document.getElementById("root")!).render(<Comparison />);
}
