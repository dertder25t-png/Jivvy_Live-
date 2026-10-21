import type { Ack, Command } from "@jivvy/protocol";
import { describe, expect, it } from "vitest";
import { docImages, upload } from "./media";

const A = "a".repeat(64);
const B = "b".repeat(64);

describe("pictures in saved documents", () => {
  it("finds the pictures a plan and a theme use, and asks the church computer for them before saving", async () => {
    const plan = { items: [{ id: "i", kind: "image", title: "x", text: "", images: [A, B, A] }] };
    const theme = { v: 1, background: { kind: "image", image: B, color: "#000000" } };
    expect(docImages(plan)).toEqual([A, B]);
    expect(docImages(theme)).toEqual([B]);
    expect(docImages({ title: "not a picture", id: "abc" })).toEqual([]);

    // The church computer already has both, so nothing needs sending and saving may go ahead.
    const asked: Command[] = [];
    const conn = {
      async send(c: Command): Promise<Ack> {
        asked.push(c);
        return { v: 1, id: "x", ok: true, missing: [] } as Ack;
      },
    };
    expect(await upload(conn as never, docImages(plan), () => {})).toBeNull();
    expect(asked).toEqual([{ type: "media.has", ids: [A, B] }]);
  });
});
