/**
 * What the service worker may touch. It caches the app's own files only, so the page opens
 * with no network. It never handles the daemon: the WebSocket channel is not a fetch, and the
 * daemon's other answers (/.well-known/jivvy, media, API) are not in the list, so they go
 * straight to the network.
 */

/** Paths under these prefixes are app files fetched on demand (pdf.js decoders and fonts). */
export const LAZY_PREFIXES = ["/pdfjs/"];

/** The one page every route is served from (static SPA fallback). */
export const SHELL = "/";

/** Pages are extensionless paths, like the daemon's web.rs; its /.well-known answers are never the app. */
export function isPagePath(pathname: string): boolean {
  if (pathname.startsWith("/.well-known/")) return false;
  return !(pathname.split("/").pop() ?? "").includes(".");
}

/** True when the worker should answer this request from its cache. Everything else is left alone. */
export function isCacheable(request: { method: string; mode?: string }, url: URL, origin: string, precached: ReadonlySet<string>): boolean {
  if (request.method !== "GET") return false;
  if (url.origin !== origin) return false;
  if (request.mode === "navigate") return isPagePath(url.pathname);
  if (precached.has(url.pathname)) return true;
  return LAZY_PREFIXES.some((p) => url.pathname.startsWith(p));
}

/** The URLs to store when the worker installs: built files and static files, plus the page. */
export function precacheList(build: readonly string[], files: readonly string[]): string[] {
  return [SHELL, ...build, ...files.filter((f) => f !== "/service-worker.js")];
}
