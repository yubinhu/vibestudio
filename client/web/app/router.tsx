import { createHashRouter } from "react-router-dom";
import { studioPath } from "@/lib/routes";
import { createAppRoutes } from "./routes";

// One-time deep-link promotion: a pre-hash `?path=/abs/skill` launch (the packaged
// app's deep link, which HashRouter ignores because it lives in location.search,
// before the `#`) becomes the hash route. Done here, at module load, so it runs
// before createHashRouter() below captures the current location.
try {
  const p = new URLSearchParams(window.location.search).get("path");
  if (p && !window.location.hash) window.location.hash = `#${studioPath(p)}`;
} catch {}

export const router = createHashRouter(createAppRoutes());
