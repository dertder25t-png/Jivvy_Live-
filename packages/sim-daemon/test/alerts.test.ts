import { describe, expect, it } from "vitest";
import { dispatch, makeEnvelope, type Alert, type Command } from "@jivvy/protocol";
import { SimDaemon } from "../src/index";

function setup() {
  const timers: Array<() => void> = [];
  const d = new SimDaemon({
    slideCount: 5,
    setTimer: (fn) => timers.push(fn),
    destinations: [{ id: "yt", name: "YouTube" }, { id: "fb", name: "Facebook" }],
  });
  const alerts: Alert[] = [];
  d.onAlert((a) => alerts.push(a));
  const send = (command: Command) => dispatch(d, makeEnvelope(command));
  return { d, send, alerts, fire: () => timers.splice(0).forEach((f) => f()) };
}

describe("stream alerts", () => {
  it("a slow upload lowers the main platform and pauses the other, with an alert for each", async () => {
    const { d, send, alerts } = setup();
    const idle = await send({ type: "state.get" });
    expect(idle.ok && idle.state?.streamDestinations, "nothing to show while not streaming").toBeFalsy();
    await send({ type: "stream.start" });
    expect(await send({ type: "state.get" })).toMatchObject({
      state: { streamDestinations: [{ id: "yt", state: "live", quality: "1080p" }, { id: "fb", state: "live", quality: "1080p" }] },
    });
    expect(alerts).toEqual([]);

    d.setUplink("slow");
    expect(alerts.map((a) => [a.kind, a.destinationId])).toEqual([["stream.lowered", "yt"], ["stream.paused", "fb"]]);
    expect(alerts[1]!.message).toBe(
      "Facebook is paused: the internet upload can't carry it right now. It comes back by itself when the upload recovers.",
    );
    expect(await send({ type: "state.get" })).toMatchObject({
      state: { stream: "live", streamDestinations: [{ id: "yt", quality: "480p" }, { id: "fb", state: "paused", quality: "" }] },
    });

    d.setUplink("ok");
    expect(alerts.at(-1)).toMatchObject({ kind: "stream.resumed", destinationId: "fb", message: "Facebook is streaming again, at 1080p." });
  });

  it("starting, stopping and restarting the stream are not alerts", async () => {
    const { d, send, alerts, fire } = setup();
    d.setUplink("slow");
    await send({ type: "stream.start" });
    d.crash();
    fire();
    await send({ type: "stream.stop" });
    expect(alerts).toEqual([]);
  });

  it("paired devices hear alerts too", async () => {
    const { d, alerts } = setup();
    await dispatch(d, makeEnvelope({ type: "stream.start" }));
    const conn = d.connect();
    conn.deviceId = "test-device"; // as if it had proven its key
    const heard: unknown[] = [];
    conn.onEvent((e) => heard.push(e));
    d.setUplink("slow");
    expect(heard).toHaveLength(alerts.length);
    expect(heard[0]).toMatchObject({ event: "alert", alert: { kind: "stream.lowered" } });
  });
});
