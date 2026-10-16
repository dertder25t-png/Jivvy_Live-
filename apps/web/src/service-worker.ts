/// <reference types="@sveltejs/kit" />
/// <reference no-default-lib="true"/>
/// <reference lib="esnext" />
/// <reference lib="webworker" />
import { build, files, version } from "$service-worker";
import { isCacheable, PDFJS_LIST, precacheList, SHELL } from "$lib/pwa";

declare const self: ServiceWorkerGlobalScope;

const CACHE = `jivvy-app-${version}`;
const urls = precacheList(build, files);
const known = new Set(urls);

self.addEventListener("install", (event) => {
  event.waitUntil(
    (async () => {
      const c = await caches.open(CACHE);
      await c.addAll(urls);
      // The pdf.js files are copied in after the build, so $service-worker doesn't list them.
      const list: string[] = await (await fetch(PDFJS_LIST)).json();
      await c.addAll(list);
      await self.skipWaiting();
    })(),
  );
});

self.addEventListener("activate", (event) => {
  event.waitUntil(
    caches.keys().then((keys) => Promise.all(keys.filter((k) => k !== CACHE).map((k) => caches.delete(k)))).then(() => self.clients.claim()),
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
