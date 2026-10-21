/**
 * Images for slides. Each picture is shrunk to fit 1920×1080 (screens never need more, and it
 * keeps uploads small), named by the SHA-256 of its bytes, kept in this browser, and sent to
 * the church computer in pieces before the run sheet that shows it.
 */
import { MEDIA_CHUNK_BYTES, type Ack } from "@jivvy/protocol";
import type { DaemonConnection } from "./daemon";
import { open, run } from "./db";

export const MAX_WIDTH = 1920;
export const MAX_HEIGHT = 1080;

const hex = (buf: ArrayBuffer) => [...new Uint8Array(buf)].map((b) => b.toString(16).padStart(2, "0")).join("");

/** The size a picture is drawn at: as large as fits MAX_WIDTH × MAX_HEIGHT, never enlarged. */
export function fitSize(w: number, h: number): { w: number; h: number } {
  const scale = Math.min(1, MAX_WIDTH / w, MAX_HEIGHT / h);
  return { w: Math.max(1, Math.round(w * scale)), h: Math.max(1, Math.round(h * scale)) };
}

/** Draws `source` at the fitted size and encodes it: JPEG for photos and PDF pages. */
export async function encode(source: CanvasImageSource, w: number, h: number): Promise<Blob> {
  const size = fitSize(w, h);
  const canvas = document.createElement("canvas");
  canvas.width = size.w;
  canvas.height = size.h;
  const ctx = canvas.getContext("2d");
  if (!ctx) throw new Error("This browser can't draw images.");
  ctx.fillStyle = "#000"; // transparent areas show black, as on the screens
  ctx.fillRect(0, 0, size.w, size.h);
  ctx.drawImage(source, 0, 0, size.w, size.h);
  const blob = await new Promise<Blob | null>((r) => canvas.toBlob(r, "image/jpeg", 0.9));
  if (!blob) throw new Error("This browser couldn't save the image.");
  return blob;
}

/** Keeps the image in this browser and returns its id. */
export async function keep(blob: Blob): Promise<string> {
  const id = hex(await crypto.subtle.digest("SHA-256", await blob.arrayBuffer()));
  await run(await open(), "media", "readwrite", (s) => s.put(blob, id));
  return id;
}

/** Shrinks and keeps a picture the operator picked. */
export async function addImage(file: File): Promise<string> {
  const bitmap = await createImageBitmap(file).catch(() => {
    throw new Error(`${file.name} isn't a picture this browser can read.`);
  });
  try {
    return await keep(await encode(bitmap, bitmap.width, bitmap.height));
  } finally {
    bitmap.close();
  }
}

export async function getImage(id: string): Promise<Blob | undefined> {
  return run<Blob | undefined>(await open(), "media", "readonly", (s) => s.get(id));
}

/** Where the church computer serves pictures (`<base>/media/<id>`), set once connected. */
let mediaBase = "";
export function setMediaSource(wsUrl: string): void {
  mediaBase = wsUrl.replace(/^ws/, "http").replace(/\/+$/, "");
  urls.clear(); // pictures that weren't found may be there now
}

/** This browser's copy, else the church computer's (kept here afterwards). */
export async function loadImage(id: string, fetcher: typeof fetch = fetch): Promise<Blob | undefined> {
  const local = await getImage(id).catch(() => undefined);
  if (local || !mediaBase) return local;
  try {
    const res = await fetcher(`${mediaBase}/media/${id}`, { signal: AbortSignal.timeout(5000) });
    if (!res.ok) return undefined;
    const blob = await res.blob();
    // The id is the hash of the bytes: anything else isn't this picture.
    if (hex(await crypto.subtle.digest("SHA-256", await blob.arrayBuffer())) !== id) return undefined;
    await run(await open(), "media", "readwrite", (s) => s.put(blob, id)).catch(() => undefined);
    return blob;
  } catch {
    return undefined;
  }
}

/** Object URLs for previews, one per image for the life of the page. */
const urls = new Map<string, Promise<string | null>>();
export function previewUrl(id: string): Promise<string | null> {
  if (!urls.has(id)) {
    const found = loadImage(id).then((b) => {
      if (b) return URL.createObjectURL(b);
      if (urls.get(id) === found) urls.delete(id); // not found now: the next render tries again
      return null;
    });
    urls.set(id, found);
  }
  return urls.get(id)!;
}

/** Every picture id (64 hex characters) named anywhere in a plan's or theme's body. */
export function docImages(body: unknown): string[] {
  const found = new Set<string>();
  const walk = (v: unknown) => {
    if (typeof v === "string") {
      if (/^[0-9a-f]{64}$/.test(v)) found.add(v);
    } else if (Array.isArray(v)) v.forEach(walk);
    else if (v && typeof v === "object") Object.values(v).forEach(walk);
  };
  walk(body);
  return [...found];
}

function base64(bytes: Uint8Array): string {
  let s = "";
  for (let i = 0; i < bytes.length; i += 0x8000) s += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  return btoa(s);
}

/**
 * Sends the church computer every image in `ids` it doesn't have yet. Resolves with null when
 * done, or a message saying what went wrong.
 */
export async function upload(
  conn: DaemonConnection,
  ids: string[],
  progress: (done: number, total: number) => void,
): Promise<string | null> {
  if (!ids.length) return null;
  const has = await conn.send({ type: "media.has", ids: [...new Set(ids)] });
  if (!has.ok) return has.message;
  const missing = has.missing ?? [];
  for (const [n, id] of missing.entries()) {
    progress(n, missing.length);
    const blob = await getImage(id);
    if (!blob) return "An image in this run sheet isn't in this browser any more. Add it again.";
    const bytes = new Uint8Array(await blob.arrayBuffer());
    for (let offset = 0; ; offset += MEDIA_CHUNK_BYTES) {
      const last = offset + MEDIA_CHUNK_BYTES >= bytes.length;
      const piece = bytes.subarray(offset, offset + MEDIA_CHUNK_BYTES);
      const ack: Ack = await conn.send({ type: "media.put", id, offset, data: base64(piece), last });
      if (!ack.ok) return ack.message;
      if (last) break;
    }
  }
  progress(missing.length, missing.length);
  return null;
}
