import { describe, expect, it } from "vitest";
import { authMessage, dispatch, makeEnvelope, PAIRING_WINDOW_MS, type Command, type DaemonEvent, type DaemonLike } from "@jivvy/protocol";
import { SimDaemon, toBase64Url } from "../src/index";

/** A device's key, made the way the web app makes it: WebCrypto P-256, not extractable. */
async function deviceKey() {
  const pair = await crypto.subtle.generateKey({ name: "ECDSA", namedCurve: "P-256" }, false, ["sign", "verify"]);
  const publicKey = toBase64Url(new Uint8Array(await crypto.subtle.exportKey("raw", pair.publicKey)));
  const sign = async (message: string) =>
    toBase64Url(new Uint8Array(await crypto.subtle.sign({ name: "ECDSA", hash: "SHA-256" }, pair.privateKey, new TextEncoder().encode(message))));
  return { publicKey, sign };
}

type Key = Awaited<ReturnType<typeof deviceKey>>;

function setup() {
  let now = 1_000_000;
  const timers: Array<() => void> = [];
  const d = new SimDaemon({ slideCount: 5, setTimer: (fn) => timers.push(fn), now: () => now });
  const church = (command: Command) => dispatch(d, makeEnvelope(command));
  const send = (conn: DaemonLike, command: Command) => dispatch(conn, makeEnvelope(command));
  return {
    d, church, send,
    advance: (ms: number) => { now += ms; timers.splice(0).forEach((f) => f()); },
    fire: () => timers.splice(0).forEach((f) => f()),
  };
}

/** Proves `key` on a new connection; returns the connection and the ack. */
async function login(d: SimDaemon, key: Key, daemonId = d.daemonId) {
  const conn = d.connect();
  const hello = await dispatch(conn, makeEnvelope({ type: "auth.hello", publicKey: key.publicKey }));
  if (!hello.ok || !hello.challenge) throw new Error("no challenge");
  const signature = await key.sign(authMessage(daemonId, hello.challenge));
  return { conn, ack: await dispatch(conn, makeEnvelope({ type: "auth.prove", signature })) };
}

/** Pairs `key` the whole way: open, request, approve on the church computer. */
async function pair(t: ReturnType<typeof setup>, key: Key, name = "Phone") {
  await t.church({ type: "pair.open" });
  const conn = t.d.connect();
  const req = await t.send(conn, { type: "pair.request", publicKey: key.publicKey, name });
  if (!req.ok || !req.request) throw new Error("request refused");
  await t.church({ type: "pair.approve", requestId: req.request.requestId });
  conn.close();
}

describe("pairing", () => {
  it("a device pairs with a code the church computer checks, then proves its key and runs the service", async () => {
    const t = setup();
    const key = await deviceKey();
    const seen: unknown[] = [];
    t.d.onPairing((p) => seen.push(p));

    const opened = await t.church({ type: "pair.open" });
    expect(opened).toMatchObject({ ok: true, pairing: { open: true, closesInMs: PAIRING_WINDOW_MS, requests: [] } });

    const phone = t.d.connect();
    const events: DaemonEvent[] = [];
    phone.onEvent((e) => events.push(e));
    const req = await t.send(phone, { type: "pair.request", publicKey: key.publicKey, name: "Sam's iPhone" });
    expect(req).toMatchObject({ ok: true, request: { code: expect.stringMatching(/^\d{6}$/) } });
    if (!req.ok || !req.request) throw new Error();

    // The church computer sees the same code the phone shows.
    expect(seen.at(-1)).toMatchObject({ open: true, requests: [{ requestId: req.request.requestId, name: "Sam's iPhone", code: req.request.code }] });
    // Asking isn't being paired.
    expect(await t.send(phone, { type: "slide.next" })).toMatchObject({ ok: false, code: "unauthorized" });

    await t.church({ type: "pair.approve", requestId: req.request.requestId });
    expect(events).toEqual([expect.objectContaining({ event: "pair.result", approved: true, daemonId: t.d.daemonId })]);

    const { conn, ack } = await login(t.d, key);
    expect(ack).toMatchObject({ ok: true, deviceId: expect.any(String), state: { slideIndex: 0 } });
    expect(await t.send(conn, { type: "slide.next" })).toMatchObject({ ok: true, state: { slideIndex: 1 } });
    expect(await t.send(conn, { type: "output.black", on: true })).toMatchObject({ ok: true });

    const list = await t.church({ type: "device.list" });
    expect(list).toMatchObject({ ok: true, devices: [{ name: "Sam's iPhone", pairedAt: 1_000_000 }] });
  });

  it("a paired device can never do the church computer's or the video process's jobs", async () => {
    const t = setup();
    const key = await deviceKey();
    await pair(t, key);
    const { conn } = await login(t.d, key);
    const refused: Command[] = [
      { type: "pair.open" }, { type: "pair.close" }, { type: "device.list" }, { type: "device.remove", deviceId: "x" },
      { type: "pair.approve", requestId: "x" }, { type: "pair.deny", requestId: "x" },
      { type: "stream.report", status: "live" }, { type: "recording.report", status: "recording" },
      { type: "media.levels", peakDb: [0], rmsDb: [0] },
    ];
    for (const command of refused) {
      expect(await t.send(conn, command), command.type).toMatchObject({ ok: false, code: "unauthorized" });
    }
  });

  it("only the right key, for this daemon, on a fresh challenge gets in", async () => {
    const t = setup();
    const key = await deviceKey();
    const stranger = await deviceKey();
    await pair(t, key);

    expect((await login(t.d, stranger)).ack).toMatchObject({ ok: false, code: "unauthorized" });
    expect((await login(t.d, key, "another-church")).ack, "signed for another daemon").toMatchObject({ ok: false, code: "unauthorized" });

    // A signature can't be used twice, or on another connection.
    const conn = t.d.connect();
    const hello = await t.send(conn, { type: "auth.hello", publicKey: key.publicKey });
    if (!hello.ok || !hello.challenge) throw new Error();
    const signature = await key.sign(authMessage(t.d.daemonId, hello.challenge));
    const other = t.d.connect();
    await t.send(other, { type: "auth.hello", publicKey: key.publicKey });
    expect(await t.send(other, { type: "auth.prove", signature })).toMatchObject({ ok: false, code: "unauthorized" });
    expect(await t.send(conn, { type: "auth.prove", signature })).toMatchObject({ ok: true });
    const again = t.d.connect();
    expect(await t.send(again, { type: "auth.prove", signature })).toMatchObject({ ok: false, code: "unauthorized" });

    // A failed proof uses up the challenge.
    const third = t.d.connect();
    const h3 = await t.send(third, { type: "auth.hello", publicKey: key.publicKey });
    if (!h3.ok || !h3.challenge) throw new Error();
    await t.send(third, { type: "auth.prove", signature: await stranger.sign(authMessage(t.d.daemonId, h3.challenge)) });
    expect(await t.send(third, { type: "auth.prove", signature: await key.sign(authMessage(t.d.daemonId, h3.challenge)) }))
      .toMatchObject({ ok: false, code: "unauthorized" });
  });

  it("removing a device ends its connection at once and it can't get back in", async () => {
    const t = setup();
    const key = await deviceKey();
    await pair(t, key);
    const { conn, ack } = await login(t.d, key);
    if (!ack.ok || !ack.deviceId) throw new Error();
    let closed = false;
    conn.onClose(() => (closed = true));

    expect(await t.church({ type: "device.remove", deviceId: ack.deviceId })).toMatchObject({ ok: true, devices: [] });
    expect(closed).toBe(true);
    expect(await t.send(conn, { type: "slide.next" })).toMatchObject({ ok: false });
    expect((await login(t.d, key)).ack).toMatchObject({ ok: false, code: "unauthorized" });
  });

  it("pairing closes by itself after two minutes, and waiting devices hear they weren't approved", async () => {
    const t = setup();
    const key = await deviceKey();
    await t.church({ type: "pair.open" });
    const phone = t.d.connect();
    const events: DaemonEvent[] = [];
    phone.onEvent((e) => events.push(e));
    const req = await t.send(phone, { type: "pair.request", publicKey: key.publicKey, name: "Phone" });
    if (!req.ok || !req.request) throw new Error();

    t.advance(PAIRING_WINDOW_MS);
    expect(events).toEqual([expect.objectContaining({ event: "pair.result", approved: false })]);
    expect(await t.church({ type: "pair.approve", requestId: req.request.requestId })).toMatchObject({ ok: false, code: "bad_arguments" });
    expect(await t.send(t.d.connect(), { type: "pair.request", publicKey: key.publicKey, name: "Phone" }))
      .toMatchObject({ ok: false, code: "unauthorized" });
    expect(await t.church({ type: "device.list" })).toMatchObject({ devices: [] });
  });

  it("a denied device isn't paired", async () => {
    const t = setup();
    const key = await deviceKey();
    await t.church({ type: "pair.open" });
    const phone = t.d.connect();
    const events: DaemonEvent[] = [];
    phone.onEvent((e) => events.push(e));
    const req = await t.send(phone, { type: "pair.request", publicKey: key.publicKey, name: "Phone" });
    if (!req.ok || !req.request) throw new Error();
    await t.church({ type: "pair.deny", requestId: req.request.requestId });
    expect(events).toEqual([expect.objectContaining({ event: "pair.result", approved: false })]);
    expect((await login(t.d, key)).ack).toMatchObject({ ok: false, code: "unauthorized" });
  });

  it("no more than five requests wait at once", async () => {
    const t = setup();
    await t.church({ type: "pair.open" });
    for (let i = 0; i < 5; i++) {
      const k = await deviceKey();
      expect(await t.send(t.d.connect(), { type: "pair.request", publicKey: k.publicKey, name: `P${i}` })).toMatchObject({ ok: true });
    }
    const k = await deviceKey();
    expect(await t.send(t.d.connect(), { type: "pair.request", publicKey: k.publicKey, name: "P6" })).toMatchObject({ ok: false, code: "unauthorized" });
  });

  it("a crash keeps paired devices; their connections drop and they prove their keys again", async () => {
    const t = setup();
    const key = await deviceKey();
    await pair(t, key);
    const { conn } = await login(t.d, key);
    await t.send(conn, { type: "slide.goto", index: 3 });
    t.d.crash();
    t.fire();
    expect(await t.send(conn, { type: "slide.next" })).toMatchObject({ ok: false });
    const back = await login(t.d, key);
    expect(back.ack).toMatchObject({ ok: true, state: { slideIndex: 3 } });
  });
});
