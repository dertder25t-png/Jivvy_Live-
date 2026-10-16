import { existsSync, readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { isCacheable, precacheList } from "./pwa";

const origin = "https://app.jivvy.org";

describe("manifest", () => {
  const manifest = JSON.parse(readFileSync(new URL("../../static/manifest.webmanifest", import.meta.url), "utf8"));
  it("is installable and points at real icons", () => {
    expect(manifest.display).toBe("standalone");
    expect(manifest.start_url).toBe("/");
    expect(manifest.icons.length).toBeGreaterThan(0);
    for (const icon of manifest.icons) expect(existsSync(new URL(`../../static${icon.src}`, import.meta.url))).toBe(true);
  });
});

describe("service worker scope", () => {
  const precached = new Set(precacheList(["/_app/immutable/a.js"], ["/brand/jivvy-app-icon.svg", "/service-worker.js"]));
  const ok = (path: string, mode?: string) => isCacheable({ method: "GET", mode }, new URL(path, origin), origin, precached);

  it("answers the app shell and its files", () => {
    expect(ok("/", "navigate")).toBe(true);
    expect(ok("/_app/immutable/a.js")).toBe(true);
    expect(ok("/pdfjs/wasm/openjpeg.wasm")).toBe(true);
  });

  it("leaves the daemon and everything else alone", () => {
    expect(isCacheable({ method: "GET" }, new URL("ws://192.168.1.20:47801/"), origin, precached)).toBe(false);
    expect(isCacheable({ method: "GET" }, new URL("https://192.168.1.20:47801/.well-known/jivvy"), origin, precached)).toBe(false);
    expect(ok("/.well-known/jivvy")).toBe(false);
    expect(isCacheable({ method: "POST", mode: "navigate" }, new URL("/", origin), origin, precached)).toBe(false);
    expect(precached.has("/service-worker.js")).toBe(false);
  });
});
