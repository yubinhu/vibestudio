import { createRoot } from "react-dom/client";
import { RouterProvider } from "react-router-dom";
import { router } from "./app/router";
import { ConfirmProvider } from "@/components/confirm";
import { initLogging } from "@/lib/log";
import { initPush } from "@/lib/push";
import { initSoftKeyboard } from "@/lib/softKeyboard";
// Self-hosted Inter (variable) — the UI font; bundled so it works offline
// in the desktop build. Falls back to system fonts via --font-geist-sans.
import "@fontsource-variable/inter";
import "./globals.css";

// Capture uncaught errors / unhandled rejections and forward warn+error to the
// backend so they land in the on-disk server log (visible in a packaged app).
initLogging();
initSoftKeyboard();
initPush();

// No StrictMode: the terminal panes attach a pty + xterm in a mount effect and
// detach/dispose on unmount, with no idempotency guard — StrictMode's double-invoke
// would double-attach and leak the first xterm instance.
createRoot(document.getElementById("root")!).render(
  <ConfirmProvider>
    <RouterProvider router={router} />
  </ConfirmProvider>,
);
