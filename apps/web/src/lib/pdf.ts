/**
 * PDF slides: each page drawn by pdf.js in this browser at the size that fills 1920×1080, then
 * kept and sent like any picture (media.ts). The church computer never sees a PDF.
 */
import { MAX_HEIGHT, MAX_WIDTH, encode, keep } from "./media";

/** Where this app serves pdf.js's decoders, fonts, character maps and colour profiles. */
const ASSETS = "/pdfjs/";

/** How much a page is scaled to fill the screen (a letter page is 612×792 points). */
export function pageScale(w: number, h: number): number {
  return Math.min(MAX_WIDTH / w, MAX_HEIGHT / h);
}

/** Loaded only when a PDF is added: pdf.js is large and most services don't need it. */
async function pdfjs() {
  const [lib, worker] = await Promise.all([import("pdfjs-dist"), import("pdfjs-dist/build/pdf.worker.min.mjs?url")]);
  lib.GlobalWorkerOptions.workerSrc = worker.default;
  return lib;
}

/** Every page of `file` as a kept image, in order. `progress` hears each page as it is done. */
export async function addPdf(file: File, progress: (page: number, pages: number) => void): Promise<string[]> {
  const lib = await pdfjs();
  const task = lib.getDocument({
    data: new Uint8Array(await file.arrayBuffer()),
    // The files pdf.js loads when a PDF needs them, served with this app (vite.config.ts).
    wasmUrl: `${ASSETS}wasm/`,
    standardFontDataUrl: `${ASSETS}standard_fonts/`,
    cMapUrl: `${ASSETS}cmaps/`,
    cMapPacked: true,
    iccUrl: `${ASSETS}iccs/`,
  });
  const doc = await task.promise.catch(async () => {
    await task.destroy();
    throw new Error(`${file.name} isn't a PDF this browser can read.`);
  });
  try {
    const ids: string[] = [];
    for (let n = 1; n <= doc.numPages; n++) {
      progress(n, doc.numPages);
      const page = await doc.getPage(n);
      const base = page.getViewport({ scale: 1 });
      const viewport = page.getViewport({ scale: pageScale(base.width, base.height) });
      const canvas = document.createElement("canvas");
      canvas.width = Math.round(viewport.width);
      canvas.height = Math.round(viewport.height);
      // Drawn as for printing: the page as it would come out of a printer, and without
      // requestAnimationFrame, which stops while the tab is in the background mid-import.
      await page.render({ canvas, viewport, intent: "print" }).promise;
      ids.push(await keep(await encode(canvas, canvas.width, canvas.height)));
      page.cleanup();
    }
    return ids;
  } finally {
    await task.destroy();
  }
}
