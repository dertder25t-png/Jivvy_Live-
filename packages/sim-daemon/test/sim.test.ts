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
    expect(await send({ type: "state.get" })).toMatchObject({ ok: true, state: { slideIndex: 2, black: false, stream: "off" } });
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
