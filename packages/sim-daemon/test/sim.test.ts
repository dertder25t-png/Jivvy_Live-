import { describe, expect, it } from "vitest";
import { dispatch, makeEnvelope } from "@jivvy/protocol";
import { SimDaemon } from "../src/index";

function setup() {
  const timers: Array<() => void> = [];
  const d = new SimDaemon({ slideCount: 5, setTimer: (fn) => timers.push(fn) });
  const send = (command: Parameters<typeof makeEnvelope>[0]) => dispatch(d, makeEnvelope(command));
  return { d, send, fire: () => timers.splice(0).forEach((f) => f()) };
}

describe("SimDaemon", () => {
  it("navigates and clamps at the ends", async () => {
    const { d, send } = setup();
    await send({ type: "slide.prev" });
    expect(d.getState().slideIndex).toBe(0);
    for (let i = 0; i < 10; i++) await send({ type: "slide.next" });
    expect(d.getState().slideIndex).toBe(4);
    expect(await send({ type: "slide.goto", index: 9 })).toMatchObject({ ok: false, code: "bad_arguments" });
  });

  it("crash restores the same slide and black state, and rejects commands meanwhile", async () => {
    const { d, send, fire } = setup();
    await send({ type: "slide.goto", index: 3 });
    await send({ type: "output.black", on: true });
    d.crash();
    expect(d.getState().restarting).toBe(true);
    expect(await send({ type: "slide.next" })).toMatchObject({ ok: false });
    fire();
    expect(d.getState()).toMatchObject({ restarting: false, slideIndex: 3, black: true });
  });

  it("crash keeps a running stream wanted and resumes it", async () => {
    const { d, send, fire } = setup();
    await send({ type: "stream.start" });
    d.crash();
    fire();
    expect(d.getState().stream).toBe("live");
  });

  it("internet outage never touches slides; stream reconnects by itself", async () => {
    const { d, send } = setup();
    await send({ type: "stream.start" });
    d.setUplink("down");
    expect(d.getState().stream).toBe("reconnecting");
    expect(await send({ type: "slide.next" })).toMatchObject({ ok: true });
    expect(d.getState().slideIndex).toBe(1);
    d.setUplink("ok");
    expect(d.getState().stream).toBe("live");
  });

  it("reports state on every ack, including state.get", async () => {
    const { send } = setup();
    await send({ type: "slide.goto", index: 2 });
    expect(await send({ type: "state.get" })).toMatchObject({ ok: true, state: { slideIndex: 2, black: false, stream: "off", streamWanted: false } });
  });

  it("recording: an engine crash never interrupts it, a video crash resumes into the same file set", async () => {
    const { d, send, fire } = setup();
    expect(await send({ type: "recording.start" })).toMatchObject({ ok: true, state: { recording: "recording", recordingWanted: true } });
    expect(d.getState().recordingParts).toBe(1);
    d.crash();
    expect(d.getState().recording).toBe("recording");
    fire();
    expect(d.getState()).toMatchObject({ recording: "recording", recordingParts: 1 });

    d.crashVideo();
    expect(d.getState().recording).toBe("waiting");
    fire();
    expect(d.getState()).toMatchObject({ recording: "recording", recordingParts: 2 });
    expect(d.snapshotForUi()).toMatchObject({ recording: "recording", recordingWanted: true });
    await send({ type: "recording.stop" });
    expect(d.snapshotForUi()).toMatchObject({ recording: "off", recordingWanted: false });
    // Stopped and started again is a new recording, not a continuation.
    await send({ type: "recording.start" });
    expect(d.getState().recordingParts).toBe(1);
  });

  it("recording: a nearly full disk warns at 10 GB and the recording carries on", async () => {
    const { d, send } = setup();
    await send({ type: "recording.start" });
    d.setDiskFree(10);
    expect(d.snapshotForUi().recordingLowDisk).toBe(false);
    d.setDiskFree(9.9);
    expect(d.snapshotForUi()).toMatchObject({ recording: "recording", recordingLowDisk: true });
    expect(await send({ type: "slide.next" })).toMatchObject({ ok: true, state: { recordingLowDisk: true } });
    d.setDiskFree(200);
    expect(d.snapshotForUi().recordingLowDisk).toBe(false);
  });

  it("recording: a low disk with no recording is no warning here (pre-flight checks the disk separately)", async () => {
    const { d } = setup();
    d.setDiskFree(1);
    expect(d.snapshotForUi().recordingLowDisk).toBe(false);
  });

  it("notifies subscribers and supports unsubscribe", async () => {
    const { d, send } = setup();
    let n = 0;
    const off = d.subscribe(() => n++);
    await send({ type: "slide.next" });
    off();
    await send({ type: "slide.next" });
    expect(n).toBe(1);
  });
});
