import {
  ack, authMessage, MAX_PENDING_PAIRINGS, nack, PAIRING_WINDOW_MS, PROTOCOL_VERSION,
  type Ack, type CommandType, type DaemonEvent, type DaemonLike, type DeviceInfo, type Envelope,
  type PairingStatus, type RecordingStatus, type StateSnapshot, type StreamStatus,
} from "@jivvy/protocol";

/**
 * In-memory stand-in for the church-computer daemon. Powers the browser demo and chaos tests.
 * It mirrors the real daemon's reliability contract: state is snapshotted, and a crash restores
 * the same slide (plan: "back on the same slide in under 3 seconds").
 */

export type { RecordingStatus, StreamStatus };

/** The recorder warns when the disk has less than this free (plan: "warns at 10 GB left"). */
export const LOW_DISK_GB = 10;
export type Uplink = "ok" | "down" | "slow";

export interface DaemonState {
  slideIndex: number;
  black: boolean;
  stream: StreamStatus;
  recording: RecordingStatus;
  uplink: Uplink;
  /** True while the engine process is "down" and restarting. */
  restarting: boolean;
  /** Free space on the recording disk, GB. */
  diskFreeGb: number;
  /** Files in the current recording. A crash starts another one in the same set, so none is ever lost. */
  recordingParts: number;
}

export interface SimOptions {
  slideCount: number;
  /** Simulated restart time after a crash, ms. Plan target: under 3000. */
  restartMs?: number;
  /** Timer hooks so tests can use fake time. */
  setTimer?: (fn: () => void, ms: number) => unknown;
  /** Clock for the pairing window, ms. */
  now?: () => number;
}

/** What a paired device may do: run the service. Settings, pairing and reports are not theirs. */
const DEVICE_COMMANDS: ReadonlySet<CommandType> = new Set<CommandType>([
  "slide.next", "slide.prev", "slide.goto", "output.black", "stream.start", "stream.stop",
  "recording.start", "recording.stop", "state.get", "state.subscribe",
]);

interface PairedDevice extends DeviceInfo {
  publicKey: string;
}

interface PendingPairing {
  requestId: string;
  name: string;
  publicKey: string;
  code: string;
  conn: DeviceConnection;
}

type Listener = (s: Readonly<DaemonState>) => void;

export class SimDaemon implements DaemonLike {
  private state: DaemonState = {
    slideIndex: 0, black: false, stream: "off", recording: "off", uplink: "ok", restarting: false,
    diskFreeGb: 500, recordingParts: 0,
  };
  /** Last persisted snapshot; survives crash(). */
  private snapshot: DaemonState = { ...this.state };
  private listeners = new Set<Listener>();
  private readonly restartMs: number;
  private readonly setTimer: (fn: () => void, ms: number) => unknown;
  private wantStream = false;
  private wantRecording = false;
  private readonly now: () => number;
  /** Stays the same through crashes, like the real daemon's. */
  readonly daemonId = randomId();
  /** Paired devices are saved like the snapshot: a crash keeps them. */
  private devices: PairedDevice[] = [];
  private pairingOpenUntil = 0;
  private pending: PendingPairing[] = [];
  private connections = new Set<DeviceConnection>();
  private pairingListeners = new Set<(p: PairingStatus) => void>();

  constructor(private readonly opts: SimOptions) {
    this.restartMs = opts.restartMs ?? 1500;
    this.setTimer = opts.setTimer ?? ((fn, ms) => setTimeout(fn, ms));
    this.now = opts.now ?? Date.now;
  }

  /** A device on the network channel. It must pair (or prove its key) before anything else. */
  connect(): DeviceConnection {
    const c = new DeviceConnection(this);
    this.connections.add(c);
    c.onClose(() => {
      this.connections.delete(c);
      // Its pairing request goes with it, like the real daemon's.
      const before = this.pending.length;
      this.pending = this.pending.filter((r) => r.conn !== c);
      if (this.pending.length !== before) this.pairingChanged();
    });
    return c;
  }

  /** The church computer's view of pairing, whenever it changes. */
  onPairing(fn: (p: PairingStatus) => void): () => void {
    this.pairingListeners.add(fn);
    return () => this.pairingListeners.delete(fn);
  }

  getState(): Readonly<DaemonState> {
    return this.state;
  }

  subscribe(fn: Listener): () => void {
    this.listeners.add(fn);
    return () => this.listeners.delete(fn);
  }

  /** A command from the church computer itself (the local channel), which is trusted. */
  handle(e: Envelope): Ack {
    if (this.state.restarting) return nack(e.id, "bad_message", "daemon is restarting");
    const c = e.command;
    switch (c.type) {
      case "pair.open":
        this.pairingOpenUntil = this.now() + PAIRING_WINDOW_MS;
        this.setTimer(() => this.pairingChanged(), PAIRING_WINDOW_MS);
        this.pairingChanged();
        return this.pairingAck(e.id);
      case "pair.close":
        this.pairingOpenUntil = 0;
        this.pairingChanged();
        return this.pairingAck(e.id);
      case "pair.approve":
      case "pair.deny": {
        const req = this.pairingOpen() ? this.pending.find((r) => r.requestId === c.requestId) : undefined;
        if (!req) return nack(e.id, "bad_arguments", "no such pairing request");
        this.pending = this.pending.filter((r) => r !== req);
        if (c.type === "pair.approve") {
          const existing = this.devices.find((d) => d.publicKey === req.publicKey);
          const deviceId = existing?.deviceId ?? randomId();
          this.devices = [
            ...this.devices.filter((d) => d !== existing),
            { deviceId, name: req.name, publicKey: req.publicKey, pairedAt: this.now() },
          ];
          req.conn.emit({ v: PROTOCOL_VERSION, event: "pair.result", approved: true, deviceId, daemonId: this.daemonId });
        } else {
          req.conn.emit({ v: PROTOCOL_VERSION, event: "pair.result", approved: false });
        }
        this.pairingChanged();
        return this.pairingAck(e.id);
      }
      case "device.list":
        return ack(e.id, undefined, { devices: this.deviceList() });
      case "device.remove": {
        const gone = this.devices.find((d) => d.deviceId === c.deviceId);
        if (!gone) return nack(e.id, "bad_arguments", "no such device");
        this.devices = this.devices.filter((d) => d !== gone);
        for (const conn of [...this.connections]) if (conn.deviceId === gone.deviceId) conn.close();
        return ack(e.id, undefined, { devices: this.deviceList() });
      }
      case "pair.request":
      case "auth.hello":
      case "auth.prove":
        return nack(e.id, "bad_arguments", "the church computer doesn't need to pair");

      case "slide.next": this.set({ slideIndex: Math.min(this.state.slideIndex + 1, this.opts.slideCount - 1) }); break;
      case "slide.prev": this.set({ slideIndex: Math.max(this.state.slideIndex - 1, 0) }); break;
      case "slide.goto":
        if (c.index >= this.opts.slideCount) return nack(e.id, "bad_arguments", "slide out of range");
        this.set({ slideIndex: c.index });
        break;
      case "output.black": this.set({ black: c.on }); break;
      case "stream.start":
        this.wantStream = true;
        this.set({ stream: this.state.uplink === "down" ? "reconnecting" : "live" });
        break;
      case "stream.stop":
        this.wantStream = false;
        this.set({ stream: "off" });
        break;
      case "recording.start":
        // Asking again changes nothing: it carries on into the same files.
        if (!this.wantRecording) this.set({ recording: "recording", recordingParts: 1 });
        this.wantRecording = true;
        break;
      case "recording.stop":
        this.wantRecording = false;
        this.set({ recording: "off", recordingParts: 0 });
        break;
      case "state.get":
      case "state.subscribe": // in memory, subscribers use subscribe(); the ack is the same
      case "media.levels": // the simulated daemon has no audio input to relay
      case "stream.report": // it simulates the stream itself (setUplink)
      case "recording.report": // and the recording (setDiskFree, crash)
        break;
    }
    return ack(e.id, this.snapshotForUi());
  }

  /** A command from a device on the network channel. @internal (use connect()). */
  async handleDevice(conn: DeviceConnection, e: Envelope): Promise<Ack> {
    if (conn.closed) return nack(e.id, "unavailable", "connection closed");
    if (this.state.restarting) return nack(e.id, "bad_message", "daemon is restarting");
    const c = e.command;
    switch (c.type) {
      case "pair.request": {
        if (!this.pairingOpen()) return nack(e.id, "unauthorized", "pairing isn't open; open it on the church computer");
        const others = this.pending.filter((r) => r.conn !== conn);
        if (others.length >= MAX_PENDING_PAIRINGS) return nack(e.id, "unauthorized", "too many pairing requests waiting");
        const req: PendingPairing = { requestId: randomId(), name: c.name, publicKey: c.publicKey, code: randomCode(), conn };
        this.pending = [...others, req];
        this.pairingChanged();
        return ack(e.id, undefined, { request: { requestId: req.requestId, code: req.code } });
      }
      case "auth.hello":
        conn.hello = { publicKey: c.publicKey, challenge: randomId(32) };
        return ack(e.id, undefined, { challenge: conn.hello.challenge, daemonId: this.daemonId });
      case "auth.prove": {
        const hello = conn.hello;
        conn.hello = undefined; // one try per challenge
        if (!hello || !(await verify(hello.publicKey, c.signature, authMessage(this.daemonId, hello.challenge))))
          return nack(e.id, "unauthorized", "not a paired device");
        // Looked up after the (async) check, so a device removed meanwhile gets nothing.
        const device = this.devices.find((d) => d.publicKey === hello.publicKey);
        if (!device || conn.closed) return nack(e.id, "unauthorized", "not a paired device");
        conn.deviceId = device.deviceId;
        return ack(e.id, this.snapshotForUi(), { deviceId: device.deviceId });
      }
    }
    if (!conn.deviceId) return nack(e.id, "unauthorized", "pair this device first");
    if (!DEVICE_COMMANDS.has(c.type)) return nack(e.id, "unauthorized", "only the church computer can do that");
    return this.handle(e);
  }

  private pairingOpen(): boolean {
    return this.now() < this.pairingOpenUntil;
  }

  private pairingStatus(): PairingStatus {
    if (!this.pairingOpen()) return { open: false, requests: [] };
    const requests = this.pending.map(({ requestId, name, code }) => ({ requestId, name, code }));
    return { open: true, closesInMs: this.pairingOpenUntil - this.now(), requests };
  }

  private pairingAck(id: string): Ack {
    return ack(id, undefined, { pairing: this.pairingStatus() });
  }

  /** Tells the church computer; when pairing has closed, waiting devices hear they weren't approved. */
  private pairingChanged(): void {
    if (!this.pairingOpen()) {
      for (const r of this.pending) r.conn.emit({ v: PROTOCOL_VERSION, event: "pair.result", approved: false });
      this.pending = [];
    }
    const status = this.pairingStatus();
    for (const l of this.pairingListeners) l(status);
  }

  private deviceList(): DeviceInfo[] {
    return this.devices.map(({ deviceId, name, pairedAt }) => ({ deviceId, name, pairedAt }));
  }

  /** The protocol's view of the state, as the real daemon reports it. */
  snapshotForUi(): StateSnapshot {
    const { slideIndex, black, stream, recording, diskFreeGb } = this.state;
    return {
      slideIndex, black, stream, streamWanted: this.wantStream,
      recording, recordingWanted: this.wantRecording, recordingLowDisk: this.wantRecording && diskFreeGb < LOW_DISK_GB,
    };
  }

  // ---- Failure injection (the demo's "failure buttons") ----

  /** Cut the internet: stream reconnects by itself when it returns; slides are unaffected. */
  setUplink(uplink: Uplink): void {
    this.set({
      uplink,
      stream: !this.wantStream ? "off" : uplink === "down" ? "reconnecting" : "live",
    });
  }

  /** Set the free space on the recording disk. Under 10 GB the state warns; the recording carries on. */
  setDiskFree(gb: number): void {
    this.set({ diskFreeGb: gb });
  }

  /** Kill the engine. It restarts from the last snapshot on the same slide. */
  crash(): void {
    if (this.state.restarting) return;
    // Connections and pairing requests die with the engine; devices reconnect and prove
    // their keys again. Paired devices are kept.
    for (const conn of [...this.connections]) conn.close();
    this.pending = [];
    this.pairingOpenUntil = 0;
    // Only the engine dies: the video process keeps writing the same recording file.
    this.state = { ...this.state, restarting: true };
    this.emit();
    this.setTimer(() => {
      this.state = { ...this.snapshot, restarting: false, uplink: this.state.uplink, diskFreeGb: this.state.diskFreeGb };
      this.state.stream = this.wantStream ? (this.state.uplink === "down" ? "reconnecting" : "live") : "off";
      this.emit();
    }, this.restartMs);
  }

  /**
   * Kill the video process. Nothing is written while it is down; it comes back writing a new
   * file in the same set, and nothing already recorded is touched.
   */
  crashVideo(): void {
    if (!this.wantRecording || this.state.recording !== "recording") return;
    this.state = { ...this.state, recording: "waiting" };
    this.emit();
    this.setTimer(() => {
      if (!this.wantRecording) return;
      this.set({ recording: "recording", recordingParts: this.state.recordingParts + 1 });
    }, this.restartMs);
  }

  private set(patch: Partial<DaemonState>): void {
    this.state = { ...this.state, ...patch };
    this.snapshot = { ...this.state, restarting: false };
    this.emit();
  }

  private emit(): void {
    for (const l of this.listeners) l(this.state);
  }
}

/** One device's connection on the network channel. */
export class DeviceConnection implements DaemonLike {
  /** @internal Set once the device has proven its key. */
  deviceId?: string;
  /** @internal The challenge from the last auth.hello. */
  hello?: { publicKey: string; challenge: string };
  closed = false;
  private listeners = new Set<(e: DaemonEvent) => void>();
  private closeListeners = new Set<() => void>();

  constructor(private readonly daemon: SimDaemon) {}

  handle(e: Envelope): Promise<Ack> {
    return this.daemon.handleDevice(this, e);
  }

  /** Events for this connection (pair.result). */
  onEvent(fn: (e: DaemonEvent) => void): () => void {
    this.listeners.add(fn);
    return () => this.listeners.delete(fn);
  }

  onClose(fn: () => void): void {
    this.closeListeners.add(fn);
  }

  /** @internal */
  emit(e: DaemonEvent): void {
    if (!this.closed) for (const l of this.listeners) l(e);
  }

  close(): void {
    if (this.closed) return;
    this.closed = true;
    for (const l of this.closeListeners) l();
  }
}

export function toBase64Url(bytes: Uint8Array): string {
  let bin = "";
  for (const b of bytes) bin += String.fromCharCode(b);
  return btoa(bin).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

export function fromBase64Url(s: string): Uint8Array<ArrayBuffer> {
  const bin = atob(s.replace(/-/g, "+").replace(/_/g, "/"));
  return Uint8Array.from(bin, (ch) => ch.charCodeAt(0));
}

function randomId(bytes = 12): string {
  return toBase64Url(crypto.getRandomValues(new Uint8Array(bytes)));
}

/** 6 digits, every code equally likely. */
function randomCode(): string {
  const n = new Uint32Array(1);
  do crypto.getRandomValues(n);
  while (n[0]! >= 4_294_000_000); // a multiple of 1,000,000
  return String(n[0]! % 1_000_000).padStart(6, "0");
}

/** ECDSA P-256 / SHA-256 over the UTF-8 message; false for a bad key or signature. */
async function verify(publicKey: string, signature: string, message: string): Promise<boolean> {
  try {
    const key = await crypto.subtle.importKey("raw", fromBase64Url(publicKey), { name: "ECDSA", namedCurve: "P-256" }, false, ["verify"]);
    const data = new TextEncoder().encode(message);
    return await crypto.subtle.verify({ name: "ECDSA", hash: "SHA-256" }, key, fromBase64Url(signature), data);
  } catch {
    return false;
  }
}
