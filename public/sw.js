/* VibeStudio service worker.
 *
 * Two jobs, both in service of the home-screen app:
 * 1. Offline app shell — tapping a notification with the tailnet VPN down must
 *    open the app's own "server unreachable" state, not a Safari error page.
 * 2. Classic Web Push fallback — on iOS 18.4+ the server's Declarative Web Push
 *    payload (web_push: 8030) is rendered by the OS and never reaches this
 *    handler; older iOS and other engines get it here and we render the same
 *    JSON ourselves.
 */
const SHELL = "vibestudio-shell-v2";
const PRECACHE = ["/", "/manifest.webmanifest", "/favicon.svg"];

self.addEventListener("install", (e) => {
  e.waitUntil(
    caches
      .open(SHELL)
      .then((c) => c.addAll(PRECACHE))
      .then(() => self.skipWaiting()),
  );
});

self.addEventListener("activate", (e) => {
  e.waitUntil(
    caches
      .keys()
      .then((keys) => Promise.all(keys.filter((k) => k !== SHELL).map((k) => caches.delete(k))))
      .then(() => self.clients.claim()),
  );
});

self.addEventListener("fetch", (e) => {
  const url = new URL(e.request.url);
  // Never intercept the API (incl. SSE streams) or the MCP gateway.
  if (e.request.method !== "GET" || url.pathname.startsWith("/api/") || url.pathname.startsWith("/gw/")) return;
  // Navigations: network first (the server's SPA fallback is authoritative),
  // cached shell only when the network is unreachable.
  if (e.request.mode === "navigate") {
    e.respondWith(
      fetch(e.request)
        .then((r) => {
          if (r.ok) {
            const copy = r.clone();
            caches.open(SHELL).then((c) => c.put("/", copy));
          }
          return r;
        })
        .catch(() => caches.match("/")),
    );
    return;
  }
  // Hashed immutable assets: cache first, fill on miss.
  if (url.pathname.startsWith("/assets/") || PRECACHE.includes(url.pathname)) {
    e.respondWith(
      caches.match(e.request).then(
        (hit) =>
          hit ||
          fetch(e.request).then((r) => {
            if (r.ok) {
              const copy = r.clone();
              caches.open(SHELL).then((c) => c.put(e.request, copy));
            }
            return r;
          }),
      ),
    );
  }
});

self.addEventListener("push", (e) => {
  let data = {};
  try {
    data = e.data ? e.data.json() : {};
  } catch {
    /* non-JSON push: show the generic banner below */
  }
  const n = data.notification || {};
  e.waitUntil(
    self.registration.showNotification(n.title || "VibeStudio", {
      body: n.body || "An agent finished a turn.",
      // Same tag as the page's web banner for this session — replace, not stack.
      tag: n.tag || undefined,
      data: { navigate: n.navigate || "/" },
    }),
  );
});

self.addEventListener("notificationclick", (e) => {
  e.notification.close();
  const target = (e.notification.data && e.notification.data.navigate) || "/";
  e.waitUntil(
    clients.matchAll({ type: "window", includeUncontrolled: true }).then((wins) => {
      for (const w of wins) {
        if ("focus" in w) {
          w.navigate(target);
          return w.focus();
        }
      }
      return clients.openWindow(target);
    }),
  );
});
