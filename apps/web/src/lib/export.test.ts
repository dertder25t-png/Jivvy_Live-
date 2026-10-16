import type { Doc } from "@jivvy/protocol";
import { describe, expect, it } from "vitest";
import { exportFiles, zip, zipNames } from "./export";

const doc = (kind: string, id: string, title: string, body: Record<string, unknown>): Doc => ({ kind, id, title, rev: 3, updatedAt: 1, body });

describe("export", () => {
  it("lists the expected files, writes song text, and drops secret fields", () => {
    const files = exportFiles([
      doc("song", "amazing", "Amazing Grace", {
        sections: [
          { id: "v1", label: "Verse 1", text: "Amazing grace" },
          { id: "c", label: "Chorus", text: "Praise" },
        ],
        arrangement: ["v1", "c", "v1"],
      }),
      doc("plan", "sun", "Sunday", { items: [{ id: "a", kind: "text", title: "Welcome", text: "Hello" }] }),
      doc("setting", "output", "Output", { fps: 30, streamKey: "abc", nested: { apiKey: "x", ok: 1 } }),
      doc("setting", "stream", "Stream", { url: "rtmp://x" }),
      doc("theme", "dark", "Dark", { bg: "#000" }),
    ]);
    const names = files.map((f) => f.path);
    expect(names).toEqual([
      "README.txt",
      "json/song/amazing.json",
      "songs/amazing-grace.txt",
      "json/plan/sun.json",
      "runsheets/sunday.txt",
      "json/setting/output.json",
      "json/theme/dark.json",
    ]);
    const text = files.find((f) => f.path === "songs/amazing-grace.txt")!.data as string;
    expect(text).toBe("Amazing Grace\n\nVerse 1\nAmazing grace\n\nChorus\nPraise\n\nOrder: Verse 1, Chorus, Verse 1\n");
    const json = files.find((f) => f.path === "json/setting/output.json")!.data as string;
    expect(json).not.toMatch(/abc|apiKey|streamKey/);
    expect(JSON.parse(json)).toMatchObject({ kind: "setting", id: "output", rev: 3, body: { fps: 30, nested: { ok: 1 } } });
    expect(zipNames(zip(files))).toEqual(names);
  });
});
