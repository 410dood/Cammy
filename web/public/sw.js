// Cammy service worker (C4 PWA). Hand-rolled, no dependency.
// Strategy: never touch /api or non-GET (live data + video must stay fresh);
// network-first for navigations (so a new build wins, offline falls back to the
// cached shell); cache-first for hashed static assets (instant, immutable).

const CACHE = "cammy-shell-v1";

self.addEventListener("install", (e) => {
  self.skipWaiting();
  e.waitUntil(
    caches.open(CACHE).then((c) => c.addAll(["/", "/index.html"]).catch(() => {})),
  );
});

self.addEventListener("activate", (e) => {
  e.waitUntil(
    caches
      .keys()
      .then((keys) => Promise.all(keys.filter((k) => k !== CACHE).map((k) => caches.delete(k))))
      .then(() => self.clients.claim()),
  );
});

self.addEventListener("fetch", (event) => {
  const req = event.request;
  if (req.method !== "GET") return;
  const url = new URL(req.url);
  if (url.origin !== self.location.origin) return;
  // Live data, media, and the WS proxy must never be served from cache.
  if (url.pathname.startsWith("/api") || url.pathname.startsWith("/sw.js")) return;

  if (req.mode === "navigate") {
    event.respondWith(
      fetch(req)
        .then((res) => {
          const copy = res.clone();
          caches.open(CACHE).then((c) => c.put("/index.html", copy));
          return res;
        })
        .catch(() => caches.match("/index.html").then((r) => r || caches.match("/"))),
    );
    return;
  }

  // Static assets: cache-first, then fill the cache.
  event.respondWith(
    caches.match(req).then(
      (hit) =>
        hit ||
        fetch(req).then((res) => {
          if (res.ok && (url.pathname.startsWith("/assets") || url.pathname.startsWith("/fonts") || url.pathname.startsWith("/icons"))) {
            const copy = res.clone();
            caches.open(CACHE).then((c) => c.put(req, copy));
          }
          return res;
        }),
    ),
  );
});

// --- Web Push (#68) --------------------------------------------------------
// The server encrypts a JSON payload {title, body, kind, event_id, ...}; show it
// as a native notification. Clicking focuses (or opens) the app.
self.addEventListener("push", (event) => {
  let data = {};
  try {
    data = event.data ? event.data.json() : {};
  } catch {
    data = { title: "Cammy", body: event.data && event.data.text() };
  }
  const title = data.title || "Cammy";
  const options = {
    body: data.body || "",
    icon: "/icons/icon-192.png",
    badge: "/icons/icon-192.png",
    tag: data.id ? `cammy-${data.id}` : undefined,
    data: { kind: data.kind, event_id: data.event_id },
  };
  event.waitUntil(self.registration.showNotification(title, options));
});

self.addEventListener("notificationclick", (event) => {
  event.notification.close();
  event.waitUntil(
    self.clients.matchAll({ type: "window", includeUncontrolled: true }).then((cls) => {
      for (const c of cls) {
        if ("focus" in c) return c.focus();
      }
      if (self.clients.openWindow) return self.clients.openWindow("/");
    }),
  );
});
