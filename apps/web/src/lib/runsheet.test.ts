import { parseRunSheet } from "@jivvy/protocol";
import { describe, expect, it } from "vitest";
import { build, imagesIn, slidesFor, type Draft, type DraftItem } from "./runsheet";
import { pageScale } from "./pdf";

describe("run sheet", () => {
  it("turns songs, scripture and a countdown into slides the daemon accepts", () => {
    const now = new Date(2026, 9, 18, 10, 0);
    const draft: Draft = {
      id: "sun",
      title: "Sunday",
      items: [
        { id: "s1", kind: "song", title: "Amazing Grace",
          text: "Amazing grace, how sweet the sound\nThat saved a wretch like me\n\nI once was lost\nbut now am found\nWas blind\nbut now\nI see" },
        { id: "r1", kind: "scripture", title: "John 3:16-17",
          text: "For God so loved the world, that he gave his only begotten Son, that whosoever believeth in him should not perish, but have everlasting life. For God sent not his Son into the world to condemn the world; but that the world through him might be saved." },
        { id: "c1", kind: "countdown", title: "Countdown", text: "Service starts in", at: "10:30" },
        { id: "l1", kind: "loop", title: "Before the service", text: "Welcome\n\nCoffee in the foyer", seconds: 8 },
        { id: "i1", kind: "image", title: "Announcements", text: "", images: ["a".repeat(64), "b".repeat(64)] },
        { id: "p1", kind: "pdf", title: "Sermon", text: "", images: ["c".repeat(64)] },
      ],
    };
    const sheet = build(draft, now);
    expect(sheet.items.map((i) => i.slides.length)).toEqual([3, 2, 1, 2, 2, 1]);
    expect(imagesIn(sheet)).toEqual(["a".repeat(64), "b".repeat(64), "c".repeat(64)]);
    // A letter-size PDF page (612×792 points) is drawn as tall as the screen: 835×1080.
    expect(Math.round(612 * pageScale(612, 792))).toBe(835);
    expect(sheet.items[3]?.advanceSeconds).toBe(8);
    expect(sheet.items[0]?.slides[1]?.lines).toEqual(["I once was lost", "but now am found", "Was blind", "but now"]);
    expect(sheet.items[1]?.slides[0]?.lines.at(-1)).toBe("John 3:16-17");
    expect(sheet.items[1]?.slides[1]?.lines.at(-2)).toBe("might be saved.");
    expect(sheet.items[2]?.slides[0]?.countdownTo).toBe(new Date(2026, 9, 18, 10, 30).getTime());
    expect(parseRunSheet(sheet)).toEqual(sheet);
    // A draft saved by a newer version, with a kind this one doesn't know, still shows its words.
    const future = { id: "f", kind: "something-new", title: "", text: "Hello\n\nWorld" } as unknown as DraftItem;
    expect(slidesFor(future, now)).toEqual([{ lines: ["Hello"] }, { lines: ["World"] }]);
    // A pasted paragraph longer than a slide line is wrapped, not cut off.
    const paragraph = "Welcome to everyone joining us this morning. ".repeat(6).trim();
    const words = slidesFor({ id: "p", kind: "text", title: "", text: paragraph }, now).flatMap((s) => s.lines);
    expect(words.join(" ")).toBe(paragraph);
  });
});
