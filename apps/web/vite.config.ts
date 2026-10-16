import { readdirSync, readFileSync, statSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, join, relative, sep } from "node:path";
import { sveltekit } from "@sveltejs/kit/vite";
import { defineConfig, type Plugin } from "vite";

/**
 * pdf.js loads some files only when a PDF needs them: decoders for JPEG 2000 and JBIG2 images
 * (wasm), the standard fonts a PDF may leave out, character maps for Asian scripts, and colour
 * profiles. They are served at /pdfjs/ (see src/lib/pdf.ts) in development and copied into the
 * build, so every PDF draws completely, with no network beyond this app.
 */
function pdfjsAssets(): Plugin {
  const root = dirname(createRequire(import.meta.url).resolve("pdfjs-dist/package.json"));
  const folders = ["wasm", "standard_fonts", "cmaps", "iccs"];
  const files = folders.flatMap((f) =>
    readdirSync(join(root, f), { recursive: true, encoding: "utf8" })
      .map((name) => join(root, f, name))
      .filter((path) => statSync(path).isFile()),
  );
  const published = (path: string) => `pdfjs/${relative(root, path).split(sep).join("/")}`;
  const byUrl = new Map(files.map((path) => [`/${published(path)}`, path]));
  return {
    name: "jivvy-pdfjs-assets",
    configureServer(server) {
      server.middlewares.use((req, res, next) => {
        const path = byUrl.get(decodeURIComponent((req.url ?? "").split("?")[0] ?? ""));
        if (!path) return next();
        res.setHeader("Content-Type", path.endsWith(".wasm") ? "application/wasm" : "application/octet-stream");
        res.end(readFileSync(path));
      });
    },
    generateBundle() {
      for (const path of files) this.emitFile({ type: "asset", fileName: published(path), source: readFileSync(path) });
      // The service worker stores these with the app so PDF slides draw offline.
      this.emitFile({ type: "asset", fileName: "pdfjs/files.json", source: JSON.stringify(files.map((f) => `/${published(f)}`)) });
    },
  };
}

export default defineConfig({
  plugins: [pdfjsAssets(), sveltekit()],
  // The daemon only accepts browsers from origins it was given (--ws-origin), so keep one port.
  server: { port: 5173, strictPort: true },
  test: { include: ["src/**/*.test.ts"] },
});
