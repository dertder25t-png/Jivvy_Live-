import { describe, expect, it } from "vitest";
import { countUpload } from "../src/lib/uploadTest";

function stream(chunks: number[]): ReadableStream<Uint8Array> {
  return new ReadableStream({
    start(c) {
      for (const n of chunks) c.enqueue(new Uint8Array(n));
      c.close();
    },
  });
}

describe("countUpload", () => {
  it("counts every byte", async () => {
    expect(await countUpload(stream([1000, 24, 0, 65536]))).toEqual({ ok: true, bytes: 66560 });
  });
  it("stops at the limit", async () => {
    const r = await countUpload(stream([600, 600]), 1000);
    expect(r).toMatchObject({ ok: false, error: "Too much data for one test." });
  });
  it("needs a body", async () => {
    expect(await countUpload(null)).toMatchObject({ ok: false });
  });
  it("a cut-off upload is an error, not a crash", async () => {
    const broken = new ReadableStream<Uint8Array>({
      start(c) {
        c.enqueue(new Uint8Array(10));
        c.error(new Error("reset"));
      },
    });
    expect(await countUpload(broken)).toMatchObject({ ok: false, error: "The upload was cut off." });
  });
});
