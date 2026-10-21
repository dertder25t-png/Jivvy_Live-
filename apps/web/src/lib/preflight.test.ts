import { readFileSync } from "node:fs";
import type { DaemonEvent, StateSnapshot } from "@jivvy/protocol";
import { describe, expect, it } from "vitest";
import { headline, overall, rows, streaming } from "./preflight";

const fixture = JSON.parse(
  readFileSync(new URL("../../../../packages/protocol/fixtures/preflight-event.json", import.meta.url), "utf8"),
) as { event: DaemonEvent; stopped: DaemonEvent };

describe("pre-flight screen", () => {
  it("lists red first with a fix, and shows the last speed measured when offline", () => {
    const e = fixture.event;
    if (e.event !== "preflight") throw new Error("fixture is a preflight event");
    const list = rows(e.preflight);
    expect(list.map((r) => [r.id, r.level])).toEqual([
      ["disk", "bad"],
      ["upload", "warn"],
      ["camera", "warn"],
      ["streaming", "ok"],
      ["stream", "ok"],
    ]);
    expect(list[0]?.fix).toContain("old recordings");
    expect(list[1]?.message).toContain("Last measured: 12.0 Mbps up");
    expect(list.find((r) => r.id === "streaming")?.message).toBe("YouTube 1080p + Facebook 720p");
    expect(overall(list)).toBe("bad");
    expect(headline(list)).toBe("1 to fix before the service, 2 worth a look.");

    // The stream starting stopped it: its own words, no internet fix, never "Ready".
    const s = fixture.stopped;
    if (s.event !== "preflight") throw new Error("fixture is a preflight event");
    const stopped = rows(s.preflight);
    expect(stopped).toHaveLength(1);
    expect(stopped[0]?.message).toContain("the stream started");
    expect(stopped[0]?.fix).toBeUndefined();
    expect(headline(stopped, s.preflight)).toMatch(/^Stopped/);
  });

  it("isn't offered while streaming", () => {
    const off: StateSnapshot = { slideIndex: 0, black: false, stream: "off" };
    expect(streaming(off)).toBe(false);
    expect(streaming({ ...off, streamWanted: true })).toBe(true);
    expect(streaming({ ...off, stream: "reconnecting" })).toBe(true);
  });
});
