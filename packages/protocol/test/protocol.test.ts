import { describe, expect, it } from "vitest";
import { PROTOCOL_VERSION, ack, dispatch, makeEnvelope, parseEnvelope, type DaemonLike } from "../src/index";

describe("parseEnvelope", () => {
  it("round-trips every command", () => {
    for (const command of [
      { type: "slide.next" }, { type: "slide.prev" }, { type: "slide.goto", index: 3 },
      { type: "output.black", on: true }, { type: "stream.start" }, { type: "stream.stop" },
    ] as const) {
      const r = parseEnvelope(JSON.stringify(makeEnvelope(command)));
      expect(r.ok && r.envelope.command).toEqual(command);
    }
  });
  it("accepts the current version and ignores unknown extra fields", () => {
    const r = parseEnvelope({ v: PROTOCOL_VERSION, id: "a", ts: 1, future: 1, command: { type: "slide.next", extra: 1 } });
    expect(r.ok).toBe(true);
  });
  it("rejects future and ancient versions with the id echoed", () => {
    expect(parseEnvelope({ v: PROTOCOL_VERSION + 1, id: "a", ts: 1, command: { type: "slide.next" } }))
      .toMatchObject({ ok: false, code: "unsupported_version", id: "a" });
    expect(parseEnvelope({ v: 0, id: "a", ts: 1, command: { type: "slide.next" } }))
      .toMatchObject({ ok: false, code: "unsupported_version" });
  });
  it("never throws on garbage", () => {
    for (const g of [null, 5, "{", "[]", {}, { v: 1 }, { v: 1, id: "a", ts: 1 }, { v: 1, id: "a", ts: 1, command: 7 }])
      expect(parseEnvelope(g).ok).toBe(false);
  });
  it("validates arguments and unknown commands", () => {
    const base = { v: 1, id: "a", ts: 1 };
    expect(parseEnvelope({ ...base, command: { type: "slide.goto", index: -1 } })).toMatchObject({ code: "bad_arguments" });
    expect(parseEnvelope({ ...base, command: { type: "slide.goto", index: 1.5 } })).toMatchObject({ code: "bad_arguments" });
    expect(parseEnvelope({ ...base, command: { type: "output.black" } })).toMatchObject({ code: "bad_arguments" });
    expect(parseEnvelope({ ...base, command: { type: "nope" } })).toMatchObject({ code: "unknown_command" });
  });
});

describe("dispatch", () => {
  const daemon: DaemonLike = { handle: (e) => ack(e.id) };
  it("acks valid commands", async () => {
    expect(await dispatch(daemon, makeEnvelope({ type: "slide.next" }, { id: "x" }))).toMatchObject({ ok: true, id: "x" });
  });
  it("nacks invalid input and survives daemon exceptions", async () => {
    expect(await dispatch(daemon, "oops")).toMatchObject({ ok: false, code: "bad_message" });
    const boom: DaemonLike = { handle: () => { throw new Error("x"); } };
    expect(await dispatch(boom, makeEnvelope({ type: "slide.next" }, { id: "y" }))).toMatchObject({ ok: false, id: "y" });
  });
});
