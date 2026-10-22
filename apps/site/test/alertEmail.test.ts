import { describe, expect, it } from "vitest";
import { serviceAlert } from "@jivvy/protocol";
import fixture from "../../../packages/protocol/fixtures/license.json";
import { KIND_WINDOW_MS, handleAlert, type AlertStore } from "../src/lib/alertEmail";

// Test values only: the fixture's published test key and its church.
const env = { LICENSE_TEST_PUBLIC_KEY: fixture.publicKey };
const church = fixture.payload.church;
const now = Date.UTC(2026, 9, 21, 10);

function store() {
  const times = new Map<string, number>();
  const sent: { to: string; subject: string; text: string }[] = [];
  const s: AlertStore = {
    churchEmail: async (id) => (id === church ? "tech@church.example" : null),
    lastSent: async (_, kind) => times.get(kind) ?? 0,
    claim: async (_, kind, t) => {
      const before = times.get(kind) ?? 0;
      if (before > t - KIND_WINDOW_MS) return null;
      times.set(kind, t);
      return before;
    },
    release: async (_, kind, previous) => void times.set(kind, previous),
    underDailyLimit: async () => true,
    send: async (to, subject, text) => void sent.push({ to, subject, text }),
  };
  return { s, sent };
}
const post = (kind: string, key: string = fixture.key) => ({ key, kind, message: "ignored: the site uses its own words" });

describe("alert emails", () => {
  it("emails the church account's address with the alert's words, and throttles a second alert of the kind", async () => {
    const { s, sent } = store();
    expect(await handleAlert(post("stream.down"), env, s, now)).toEqual({ status: 200, body: { ok: true, sent: true } });
    expect(sent).toHaveLength(1);
    expect(sent[0]!.to).toBe("tech@church.example");
    expect(sent[0]!.text).toContain(serviceAlert("stream.down").message);
    // Again within 10 minutes: nothing more is sent. After them it is.
    expect((await handleAlert(post("stream.down"), env, s, now + 60_000)).body).toEqual({ ok: true, sent: false, reason: "throttled" });
    expect(sent).toHaveLength(1);
    expect((await handleAlert(post("stream.down"), env, s, now + KIND_WINDOW_MS + 1)).body.sent).toBe(true);
  });

  it("sends a back email only after its down email, once", async () => {
    const { s, sent } = store();
    expect((await handleAlert(post("audio.back"), env, s, now)).body).toEqual({ ok: true, sent: false, reason: "no down email" });
    await handleAlert(post("audio.silent"), env, s, now + 1);
    expect((await handleAlert(post("audio.back"), env, s, now + 2)).body.sent).toBe(true);
    expect((await handleAlert(post("audio.back"), env, s, now + KIND_WINDOW_MS + 3)).body.sent).toBe(false);
    expect(sent).toHaveLength(2);
  });

  it("refuses a bad or unknown key, and kinds that are not emailed", async () => {
    const { s, sent } = store();
    const bad = fixture.bad[0]!.key;
    expect((await handleAlert(post("stream.down", bad), env, s, now)).status).toBe(401);
    expect((await handleAlert(post("stream.lowered"), env, s, now)).status).toBe(400);
    expect((await handleAlert(post("stream.down"), {}, s, now)).status).toBe(503);
    expect(sent).toHaveLength(0);
  });
});
