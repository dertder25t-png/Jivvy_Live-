import { describe, expect, it } from "vitest";
import { balance, parseLyrics, sectionHeader, sung } from "../src/index";
import { HYMNS, hymnSong } from "../src/hymns";

describe("songs", () => {
  it("finds sections in pasted lyrics and sings them in order", () => {
    expect(["Verse 1", "[Chorus]", "CHORUS:", "v2", "Pre-Chorus", "(Repeat Chorus)"].map((l) => sectionHeader(l)?.label)).toEqual(
      ["Verse 1", "Chorus", "Chorus", "Verse 2", "Pre-Chorus", "Chorus"],
    );
    expect(sectionHeader("Chorus x2")).toEqual({ label: "Chorus", repeat: 2 });
    expect(sectionHeader("Was blind, but now I see")).toBeNull();

    // Headers, a chorus named again with nothing under it, and a bridge sung twice.
    const song = { title: "", ...parseLyrics("Verse 1\na\nb\n\nChorus\nc\nd\n\nVerse 2\ne\n\nChorus\n\nBridge x2\nf") };
    expect(song.sections.map((s) => s.label)).toEqual(["Verse 1", "Chorus", "Verse 2", "Bridge"]);
    expect(song.arrangement).toEqual(["verse-1", "chorus", "verse-2", "chorus", "bridge", "bridge"]);

    // No headers: a paragraph that comes back word for word is the chorus.
    const plain = parseLyrics("one\n\nla la\n\ntwo\n\nLa, la!");
    expect(plain.sections.map((s) => s.label)).toEqual(["Verse 1", "Chorus", "Verse 2"]);
    expect(plain.arrangement).toEqual(["verse-1", "chorus", "verse-2", "chorus"]);

    // Six lines are two slides of three.
    expect(balance([1, 2, 3, 4, 5, 6], 4)).toEqual([[1, 2, 3], [4, 5, 6]]);
  });

  it("sings a hymn's refrain, printed once, after every verse", () => {
    for (const hymn of HYMNS) expect(hymnSong(hymn).sections.length).toBeGreaterThan(0);
    const hymn = HYMNS.find((h) => h.id === "blessed-assurance");
    const song = hymnSong(hymn!);
    expect(sung(song).map((s) => s.label)).toEqual(["Verse 1", "Refrain", "Verse 2", "Refrain", "Verse 3", "Refrain"]);
    expect(song.copyright).toBe("Public domain");
  });
});
