import { createRoot } from "react-dom/client";
import { createHashRouter, RouterProvider } from "react-router-dom";
import { createAppRoutes } from "@/app/routes";
import { ConfirmProvider } from "@/components/confirm";
import { initSoftKeyboard } from "@/lib/softKeyboard";

// This separate HTML entry shares the app's routes and HTTP backend. Each frame
// creates its own router with an optional experimental home component.
const proposed = new URLSearchParams(window.location.search).get("view") === "proposed";
const router = createHashRouter(createAppRoutes(
  proposed ? () => import("./DashboardRoute") : undefined,
));

initSoftKeyboard();
createRoot(document.getElementById("root")!).render(
  <ConfirmProvider>
    <RouterProvider router={router} />
  </ConfirmProvider>,
);
