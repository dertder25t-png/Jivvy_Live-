/// <reference types="@sveltejs/kit" />
/// <reference no-default-lib="true"/>
/// <reference lib="esnext" />
/// <reference lib="webworker" />
import { build, files, version } from "$service-worker";
import { isCacheable, precacheList, SHELL } from "$lib/pwa";
import type { PushPayload } from "@jivvy/protocol";

declare const self: ServiceWorkerGlobalScope;

const CACHE = `jivvy-app-${version}`;
const urls = precacheList(build, files);
const known = new Set(urls);

self.addEventListener("install", (event) => {
  // Only the app itself (pdf.js's module and worker are in the build). The optional /pdfjs/ data
  // (wasm decoders, fonts, character maps) is stored on first use, so it can never fail the install.
  event.waitUntil(caches.open(CACHE).then((c) => c.addAll(urls)).then(() => self.skipWaiting()));
});

self.addEventListener("activate", (event) => {
  event.waitUntil(
    caches.keys().then((keys) => Promise.all(keys.filter((k) => k !== CACHE).map((k) => caches.delete(k)))).then(() => self.clients.claim()),
  );
});

// Alerts from the church computer (web push): only the alert's words and a tag.
self.addEventListener("push", (event) => {
  let alert: Partial<PushPayload> = {};
  try {
    alert = event.data?.json() ?? {};
  } catch {
    // Not ours to read; still say something, as browsers require for a visible push.
  }
  const body = typeof alert.message === "string" ? alert.message : "Something needs a look on the church computer.";
  event.waitUntil(self.registration.showNotification("Jivvy Live", { body, tag: alert.tag || "alert", icon: "/brand/jivvy-app-icon.svg" }));
});

self.addEventListener("notificationclick", (event) => {
  event.notification.close();
  event.waitUntil(
    (async () => {
      const open = await self.clients.matchAll({ type: "window", includeUncontrolled: true });
      const mine = open.find((c) => new URL(c.url).origin === self.location.origin);
      if (mine) await mine.focus();
      else await self.clients.openWindow("/");
    })(),
  );
});

self.addEventListener("fetch", (event) => {
  const { request } = event;
  const url = new URL(request.url);
  // Not ours (the daemon, other origins, non-GET): no respondWith, so it goes straight out.
  if (!isCacheable(request, url, self.location.origin, known)) return;
  event.respondWith(
    (async () => {
      const cache = await caches.open(CACHE);
      const hit = await cache.match(request.mode === "navigate" ? SHELL : url.pathname);
      if (hit) return hit;
      const res = await fetch(request);
      if (res.ok && !known.has(url.pathname) && request.mode !== "navigate") await cache.put(url.pathname, res.clone());
      return res;
    })(),
  );
});
