import { Navigate, type RouteObject } from "react-router-dom";
import AppShell from "./AppShell";
import RootFallback from "./RootFallback";
import RenamedRoute from "./RenamedRoute";

/** Shared route definitions; the comparison frame can substitute only home. */
export function createAppRoutes(
  homeLoader: NonNullable<RouteObject["lazy"]> = () => import("@/pages/home/DashboardRoute"),
): RouteObject[] {
  return [
    {
      element: <AppShell />,
      HydrateFallback: RootFallback,
      children: [
        { index: true, lazy: homeLoader },
        // The skill gallery is embedded on the home dashboard now; /skills redirects
        // there (the per-skill editor still lives at /skills/:root below).
        { path: "skills", element: <Navigate to="/" replace /> },
        { path: "connectors", lazy: () => import("@/pages/credentials/CredentialsRoute") },
        { path: "mining", lazy: () => import("@/pages/mining/MiningRoute") },
        // The Sessions UI is the always-mounted host in AppShell; this route only
        // owns the URL/visibility, so its own element renders nothing.
        { path: "sessions", element: null },
        {
          path: "skills/:root",
          lazy: () => import("@/pages/studio/StudioRoute"),
          children: [
            { index: true, lazy: () => import("@/pages/studio/StudioIndexRoute") },
            { path: "file/*", lazy: () => import("@/pages/studio/StudioFileRoute") },
            { path: "commit/:sha", lazy: () => import("@/pages/studio/StudioCommitRoute") },
          ],
        },
        // Loose-markdown editor: open/edit any .md by absolute path. Standalone (no
        // StudioContext/git/skill chrome) — it only shares AppShell + the editor.
        { path: "markdown/:path", lazy: () => import("@/pages/markdown/MarkdownRoute") },
        // Back-compat redirects from the pre-rename URLs (studio → skills, etc.).
        { path: "credentials", element: <RenamedRoute to="connectors" /> },
        { path: "secrets", element: <RenamedRoute to="connectors" /> },
        { path: "terminals", element: <RenamedRoute to="sessions" /> },
        { path: "studio/*", element: <RenamedRoute to="skills" /> },
        { path: "*", element: <Navigate to="/" replace /> },
      ],
    },
  ];
}
