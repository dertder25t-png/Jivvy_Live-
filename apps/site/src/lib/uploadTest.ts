/**
 * Upload test for the church computer's pre-flight check: it sends a few seconds of data and
 * times it; this side only counts what arrives and throws it away. Nothing is stored, and no
 * service depends on it (the daemon falls back to the last speed it measured).
 */

/** Most bytes one test may send: about 5 s at 50 Mbps. More is cut off, so it can't be used to soak bandwidth. */
export const MAX_UPLOAD_BYTES = 32 * 1024 * 1024;

export type UploadCount = { ok: true; bytes: number } | { ok: false; bytes: number; error: string };

/** Reads and discards `body`, counting bytes, stopping at `max`. */
export async function countUpload(body: ReadableStream<Uint8Array> | null, max = MAX_UPLOAD_BYTES): Promise<UploadCount> {
  if (!body) return { ok: false, bytes: 0, error: "No data sent." };
  const reader = body.getReader();
  let bytes = 0;
  try {
    for (;;) {
      const { done, value } = await reader.read();
      if (done) return { ok: true, bytes };
      bytes += value.byteLength;
      if (bytes > max) {
        await reader.cancel();
        return { ok: false, bytes, error: "Too much data for one test." };
      }
    }
  } catch {
    return { ok: false, bytes, error: "The upload was cut off." };
  }
}
