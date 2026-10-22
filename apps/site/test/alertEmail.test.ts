import { describe, expect, it } from "vitest";
import { serviceAlert } from "@jivvy/protocol";
import fixture from "../../../packages/protocol/fixtures/license.json";
import { KIND_WINDOW_MS, handleAlert, type AlertStore } from "../src/lib/alertEmail";

// Test values only: the fixture's published test key and its church.

const church = fixture.payload.church;
const now = Date.UTC(2026, 9, 21, 10);

function store(issued: string[] = [fixture.key]) {
  const times = new Map<string, number>();
  const sent: { to: string; subject: string; text: string }[] = [];
  const day = { count: 0 };
  const fail = { send: false, limit: false };
  const s: AlertStore = {
    issuedKeys: async (id) => (id === church ? issued : []),
    churchEmail: async (id) => (id === church ? "tech@church.example" : null),
    lastSent: async (_, kind) => times.get(kind) ?? 0,
    claim: async (_, kind, t) => {
      const before = times.get(kind) ?? 0;
      if (before > t - KIND_WINDOW_MS) return null;
      times.set(kind, t);
      return before;
    },
    release: async (_, kind, previous) => void times.set(kind, previous),
    underDailyLimit: async () => {
      if (fail.limit) throw new Error("db down");
      return ++day.count <= 20;
    },
    refundDaily: async () => void (day.count = Math.max(0, day.count - 1)),
    send: async (to, subject, text) => {
      if (fail.send) throw new Error("mail down");
      sent.push({ to, subject, text });
    },
  };
  return { s, sent, times, day, fail };
}
const post = (kind: string, key: string = fixture.key) => ({ key, kind, message: "ignored: the site uses its own words" });

describe("alert emails", () => {
  it("emails the church account's address with the alert's words, and throttles a second alert of the kind", async () => {
    const { s, sent } = store();
    expect(await handleAlert(post("stream.down"), s, now)).toEqual({ status: 200, body: { ok: true, sent: true } });
    expect(sent).toHaveLength(1);
    expect(sent[0]!.to).toBe("tech@church.example");
    expect(sent[0]!.text).toContain(serviceAlert("stream.down").message);
    // Again within 10 minutes: nothing more is sent. After them it is.
    expect((await handleAlert(post("stream.down"), s, now + 60_000)).body).toEqual({ ok: true, sent: false, reason: "throttled" });
    expect(sent).toHaveLength(1);
    expect((await handleAlert(post("stream.down"), s, now + KIND_WINDOW_MS + 1)).body.sent).toBe(true);
  });

  it("sends a back email only after its down email, once", async () => {
    const { s, sent } = store();
    expect((await handleAlert(post("audio.back"), s, now)).body).toEqual({ ok: true, sent: false, reason: "no down email" });
    await handleAlert(post("audio.silent"), s, now + 1);
    expect((await handleAlert(post("audio.back"), s, now + 2)).body.sent).toBe(true);
    expect((await handleAlert(post("audio.back"), s, now + KIND_WINDOW_MS + 3)).body.sent).toBe(false);
    expect(sent).toHaveLength(2);
  });

  it("a failed send or limit check leaves the kind's slot and the day's count as they were", async () => {
    const { s, sent, times, day, fail } = store();
    for (const which of ["send", "limit"] as const) {
      fail[which] = true;
      expect((await handleAlert(post("stream.down"), s, now)).status).toBe(502);
      expect([times.get("stream.down") ?? 0, day.count]).toEqual([0, 0]);
      fail[which] = false;
    }
    // So the retry is neither throttled nor short of quota.
    expect((await handleAlert(post("stream.down"), s, now + 1)).body.sent).toBe(true);
    expect([sent.length, day.count]).toEqual([1, 1]);
  });

  it("refuses a bad key, a correctly signed but unrecorded one, and kinds that are not emailed", async () => {
    const { s, sent } = store();
    const bad = fixture.bad[0]!.key;
    expect((await handleAlert(post("stream.down", bad), s, now)).status).toBe(401);
    expect((await handleAlert(post("stream.lowered"), s, now)).status).toBe(400);
    // Correctly signed (the test key is public) but never issued or recorded by this site: refused.
    const unrecorded = store([]);
    expect((await handleAlert(post("stream.down"), unrecorded.s, now)).status).toBe(401);
    expect(unrecorded.sent).toHaveLength(0);
    expect(sent).toHaveLength(0);
  });
});
