import { describe, expect, it } from "vitest";
import type { SyncStatus } from "@jivvy/protocol";
import fixture from "../../../../packages/protocol/fixtures/sync-status.json";
import { noticeLine, syncLine } from "./sync";

describe("sync status line", () => {
  it("says where sync stands and keeps both versions of a song in plain words", () => {
    const s = fixture.ack.sync as SyncStatus;
    expect(syncLine(s, () => "10:30")).toBe("Offline: changes are saved here and sync when the internet is back. 2 changes waiting.");
    expect(syncLine({ ...s, state: "ok", pending: 0 }, () => "10:30")).toBe("Synced at 10:30.");
    expect(syncLine({ ...s, state: "paused_for_service", pending: 1 }, () => "")).toBe("Sync paused during the service. 1 change waiting.");
    expect(noticeLine(s.changedElsewhere[0]!)).toBe('"Amazing Grace" was changed here and elsewhere: both versions are kept.');
  });
});
