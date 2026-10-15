/**
 * One connection to the church computer's daemon over its WebSocket channel: proves this
 * browser's device key (or pairs it first), subscribes to the state, and sends commands.
 * Reconnects by itself until closed.
 */
import {
  authMessage,
  makeEnvelope,
  MAX_MESSAGE_BYTES,
  messageBytes,
  type Ack,
  type Command,
  type DaemonEvent,
  type DocInfo,
  type Levels,
  type RunSheet,
  type StateSnapshot,
} from "@jivvy/protocol";
import { deviceKey, publicKey, sign } from "./keys";

export type Status =
  | { kind: "connecting" }
  /** Reached the daemon, but this browser isn't paired with it. */
  | { kind: "unpaired"; message?: string }
  /** Waiting for the church computer to approve; it shows the same code. */
  | { kind: "pairing"; code: string; requestId: string }
  | { kind: "ready" }
  | { kind: "offline"; message: string };

export interface Handlers {
  status(s: Status): void;
  state(s: StateSnapshot): void;
  /** The daemon's run sheet, whenever it changes (null when none is set). */
  runSheet(r: RunSheet | null): void;
  /** Live audio input levels, about ten a second, for the meter. */
  levels?(l: Levels): void;
  /** A document in the church computer's library was saved or deleted, here or elsewhere. */
  docChanged?(doc: DocInfo, deleted: boolean): void;
}

/**
 * Whether this browser is paired with the daemon at this address. Kept in localStorage; where
 * the browser blocks storage, in memory for this visit (the device key is then per visit too).
 */
const pairedKey = (url: string) => `jivvy.paired:${url}`;
const pairedInMemory = new Map<string, string>();
const paired = {
  get(url: string): string | null {
    let stored: string | null = null;
    try {
      stored = localStorage.getItem(pairedKey(url));
    } catch {
      // blocked storage: memory only
    }
    // Also when the write failed but reads work (storage full): memory has it for this visit.
    return stored ?? pairedInMemory.get(url) ?? null;
  },
  set(url: string, deviceId: string): void {
    pairedInMemory.set(url, deviceId);
    try {
      localStorage.setItem(pairedKey(url), deviceId);
    } catch {
      // blocked storage: remembered for this visit
    }
  },
  remove(url: string): void {
    pairedInMemory.delete(url);
    try {
      localStorage.removeItem(pairedKey(url));
    } catch {
      // nothing stored
    }
  },
};

const failed = (message: string): Ack => ({ v: 1, id: "", ok: false, code: "unavailable", message });

export class DaemonConnection {
  private ws: WebSocket | null = null;
  private waiting = new Map<string, (ack: Ack) => void>();
  private closed = false;
  private retry: ReturnType<typeof setTimeout> | null = null;
  private rev: number | undefined;
  /** A state event arrived since subscribing: newer than the snapshot on the subscribe ack. */
  private sawEvent = false;

  constructor(
    readonly url: string,
    private h: Handlers,
  ) {}

  connect(): void {
    this.closed = false;
    this.h.status({ kind: "connecting" });
    let ws: WebSocket;
    try {
      ws = new WebSocket(this.url);
    } catch {
      this.h.status({ kind: "offline", message: "That isn't a WebSocket address (ws:// or wss://)." });
      return;
    }
    this.ws = ws;
    ws.onopen = () => void this.start(ws);
    ws.onmessage = (m) => this.receive(String(m.data));
    ws.onclose = () => {
      if (this.ws !== ws) return;
      this.ws = null;
      for (const done of this.waiting.values()) done(failed("disconnected"));
      this.waiting.clear();
      if (this.closed) return;
      this.h.status({ kind: "offline", message: "Can't reach the church computer. Trying again…" });
      this.retry = setTimeout(() => this.connect(), 2000);
    };
  }

  close(): void {
    this.closed = true;
    if (this.retry) clearTimeout(this.retry);
    this.ws?.close();
    this.ws = null;
    // Anything still waiting (an upload mid-way) hears now, rather than never.
    for (const done of this.waiting.values()) done(failed("disconnected"));
    this.waiting.clear();
  }

  /** Sends a command and resolves with its ack (a failed ack when disconnected). */
  send(command: Command): Promise<Ack> {
    return this.sendOn(this.ws, command);
  }

  /**
   * Sends on `ws` only if it is still this connection's socket: a step begun on a socket that
   * has since closed must never land on the next one (an old auth.prove would spoil its sign-in).
   */
  private sendOn(ws: WebSocket | null, command: Command): Promise<Ack> {
    if (!ws || ws !== this.ws || ws.readyState !== WebSocket.OPEN) return Promise.resolve(failed("not connected"));
    const env = makeEnvelope(command);
    // The daemon would refuse it and close the connection; say so here instead.
    const bytes = messageBytes(env);
    if (bytes > MAX_MESSAGE_BYTES) {
      const kb = (n: number) => Math.ceil(n / 1024);
      return Promise.resolve(
        failed(`This is too big to send (${kb(bytes)} KB; the limit is ${kb(MAX_MESSAGE_BYTES)} KB). Remove some items.`),
      );
    }
    return new Promise((resolve) => {
      this.waiting.set(env.id, resolve);
      ws.send(JSON.stringify(env));
    });
  }

  /** Asks to pair; the church computer approves after checking the code matches. */
  async pair(name: string): Promise<void> {
    const key = await deviceKey();
    const ack = await this.send({ type: "pair.request", publicKey: await publicKey(key), name });
    if (!ack.ok) return this.h.status({ kind: "unpaired", message: ack.message });
    if (ack.request) this.h.status({ kind: "pairing", ...ack.request });
  }

  private receive(text: string): void {
    let msg: Ack | DaemonEvent;
    try {
      msg = JSON.parse(text);
    } catch {
      return;
    }
    if ("event" in msg) return this.event(msg);
    const done = this.waiting.get(msg.id);
    this.waiting.delete(msg.id);
    done?.(msg);
  }

  private event(e: DaemonEvent): void {
    if (e.event === "state") {
      this.sawEvent = true;
      this.gotState(e.state);
    }
    else if (e.event === "pair.result") {
      if (e.approved) {
        paired.set(this.url, e.deviceId ?? "paired");
        void this.start(this.ws);
      } else {
        this.h.status({ kind: "unpaired", message: "The church computer didn't approve this device." });
      }
    }
    else if (e.event === "levels") this.h.levels?.(e.levels);
    else if (e.event === "doc.changed") this.h.docChanged?.(e.doc, e.deleted === true);
    // Other events (alerts, ...) come with the screens that show them.
  }

  private gotState(s: StateSnapshot): void {
    this.h.state(s);
    if (s.runSheetRev === this.rev) return;
    this.rev = s.runSheetRev;
    void this.send({ type: "runsheet.get" }).then((a) => {
      if (a.ok) this.h.runSheet(a.runSheet ?? null);
    });
  }

  /** Proves this device's key on `ws`, then follows the state. Stops if `ws` is replaced meanwhile. */
  private async start(ws: WebSocket | null): Promise<void> {
    const current = () => ws !== null && ws === this.ws;
    if (!paired.get(this.url)) return this.h.status({ kind: "unpaired" });
    const key = await deviceKey();
    const hello = await this.sendOn(ws, { type: "auth.hello", publicKey: await publicKey(key) });
    // Disconnected mid-way (or this socket was replaced): the reconnect tries again, still paired.
    if (!current() || (!hello.ok && hello.code === "unavailable")) return;
    if (!hello.ok || !hello.challenge || !hello.daemonId) return this.h.status({ kind: "unpaired" });
    const signature = await sign(key, authMessage(hello.daemonId, hello.challenge));
    const proof = await this.sendOn(ws, { type: "auth.prove", signature });
    if (!current() || (!proof.ok && proof.code !== "unauthorized")) return;
    if (!proof.ok) {
      // Removed on the church computer, or another daemon at this address: pair again.
      paired.remove(this.url);
      return this.h.status({ kind: "unpaired", message: "This device isn't paired with this church computer." });
    }
    this.rev = undefined;
    this.sawEvent = false;
    const sub = await this.sendOn(ws, { type: "state.subscribe" });
    if (!current()) return;
    if (!sub.ok) return this.h.status({ kind: "offline", message: sub.message });
    this.h.status({ kind: "ready" });
    // The daemon sends events queued during the subscribe ahead of its ack, so an event already
    // seen is newer than the ack's snapshot: keep it.
    if (sub.state && !this.sawEvent) this.gotState(sub.state);
  }
}
