import { describe, expect, it } from "vitest";
import { dispatch, makeEnvelope, type Command, type Preflight } from "@jivvy/protocol";
import { PREFLIGHT_MS, SimDaemon } from "../src/index";

function setup() {
  let now = 1_000;
  const timers: Array<() => void> = [];
  const d = new SimDaemon({
    slideCount: 5,
    setTimer: (fn) => timers.push(fn),
    now: () => now,
    destinations: [{ id: "yt", name: "YouTube" }, { id: "fb", name: "Facebook" }],
  });
  const results: Preflight[] = [];
  d.onPreflight((p) => results.push(p));
  const send = (command: Command) => dispatch(d, makeEnvelope(command));
  const finish = () => { now += PREFLIGHT_MS; timers.splice(0).forEach((f) => f()); };
  return { d, send, results, finish };
}

describe("pre-flight streaming summary", () => {
  it("measures the upload and says what each platform will get", async () => {
    const { send, results, finish } = setup();
    expect(await send({ type: "stream.preflight" })).toMatchObject({ ok: true, preflight: { status: "measuring" } });
    finish();
    expect(results).toEqual([expect.objectContaining({ status: "done", measured: true, summary: "YouTube 1080p + Facebook 1080p" })]);
  });

  it("a slow upload: the main platform lower, the other paused, said plainly", async () => {
    const { d, send, results, finish } = setup();
    d.setUplink("slow");
    await send({ type: "stream.preflight" });
    finish();
    expect(results[0]).toMatchObject({ uploadKbps: 1500, summary: "YouTube 480p. Facebook won't fit and would be paused." });
  });

  it("with no internet it falls back to the last speed measured, and says so", async () => {
    const { d, send, results, finish } = setup();
    await send({ type: "stream.preflight" });
    finish();
    d.setUplink("down");
    await send({ type: "stream.preflight" });
    finish();
    expect(results[1]).toMatchObject({ status: "done", measured: false, measuredAt: 1_000 + PREFLIGHT_MS, uploadKbps: 20_000 });
    expect(results[1]!.message).toContain("last speed measured");
  });

  it("with no internet and nothing measured before, it fails in plain words", async () => {
    const { d, send, results, finish } = setup();
    d.setUplink("down");
    await send({ type: "stream.preflight" });
    finish();
    expect(results[0]).toMatchObject({ status: "failed", message: "Couldn't test the upload: check the internet connection." });
  });

  it("never runs while streaming", async () => {
    const { send } = setup();
    await send({ type: "stream.start" });
    expect(await send({ type: "stream.preflight" })).toMatchObject({ ok: false, code: "unavailable" });
  });
});
