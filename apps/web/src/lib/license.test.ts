import { describe, expect, it } from "vitest";
import type { StateSnapshot } from "@jivvy/protocol";
import { maybeInService } from "./license";

describe("license notice", () => {
  it("is hidden as soon as a service starts", () => {
    const idle: StateSnapshot = { slideIndex: 0, black: false, stream: "off" };
    expect(maybeInService(idle)).toBe(false);
    expect(maybeInService({ ...idle, streamWanted: true })).toBe(true);
    expect(maybeInService({ ...idle, recordingWanted: true })).toBe(true);
    expect(maybeInService({ ...idle, runSheetId: "sunday" })).toBe(true);
    expect(maybeInService({ ...idle, runSheetId: "sunday", black: true })).toBe(false);
  });
});
