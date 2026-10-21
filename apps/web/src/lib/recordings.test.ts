import { readFileSync } from "node:fs";
import type { RecordingSet } from "@jivvy/protocol";
import { describe, expect, it } from "vitest";
import { duration, size, title } from "./recordings";

const fixture = JSON.parse(
  readFileSync(new URL("../../../../packages/protocol/fixtures/recordings-list.json", import.meta.url), "utf8"),
) as { ack: { recordings: RecordingSet[] } };

describe("recordings list", () => {
  it("labels a set by its plan in local time, and an old set by its date and time", () => {
    const [labelled, old] = fixture.ack.recordings as [RecordingSet, RecordingSet];
    expect(title(labelled, "UTC")).toBe("Sunday 9:30 · Oct 10, 2026 · 1 h 12 min · Waiting to convert to MP4");
    expect(title(old, "UTC")).toBe("Recording · Oct 4, 2026, 3:00 PM · 1 h 10 min · MP4 ready");
    // The same moment in another time zone reads differently.
    expect(title(old, "America/Chicago")).toContain("Oct 4, 2026, 10:00 AM");
    expect(size(labelled.bytes)).toBe("5.0 GB");
    expect(duration(3600)).toBe("1 h");
  });
});
