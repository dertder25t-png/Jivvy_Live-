import { ack, nack, type Ack, type DaemonLike, type Envelope } from "@jivvy/protocol";

/**
 * In-memory stand-in for the church-computer daemon. Powers the browser demo and chaos tests.
 * It mirrors the real daemon's reliability contract: state is snapshotted, and a crash restores
 * the same slide (plan: "back on the same slide in under 3 seconds").
 */

export type StreamStatus = "off" | "live" | "reconnecting";
export type Uplink = "ok" | "down" | "slow";

export interface DaemonState {
  slideIndex: number;
  black: boolean;
  stream: StreamStatus;
  uplink: Uplink;
  /** True while the engine process is "down" and restarting. */
  restarting: boolean;
}

export interface SimOptions {
  slideCount: number;
  /** Simulated restart time after a crash, ms. Plan target: under 3000. */
  restartMs?: number;
  /** Timer hooks so tests can use fake time. */
  setTimer?: (fn: () => void, ms: number) => unknown;
}

type Listener = (s: Readonly<DaemonState>) => void;

export class SimDaemon implements DaemonLike {
  private state: DaemonState = { slideIndex: 0, black: false, stream: "off", uplink: "ok", restarting: false };
  /** Last persisted snapshot; survives crash(). */
  private snapshot: DaemonState = { ...this.state };
  private listeners = new Set<Listener>();
  private readonly restartMs: number;
  private readonly setTimer: (fn: () => void, ms: number) => unknown;
  private wantStream = false;

  constructor(private readonly opts: SimOptions) {
    this.restartMs = opts.restartMs ?? 1500;
    this.setTimer = opts.setTimer ?? ((fn, ms) => setTimeout(fn, ms));
  }

  getState(): Readonly<DaemonState> {
    return this.state;
  }

  subscribe(fn: Listener): () => void {
    this.listeners.add(fn);
    return () => this.listeners.delete(fn);
  }

  handle(e: Envelope): Ack {
    if (this.state.restarting) return nack(e.id, "bad_message", "daemon is restarting");
    const c = e.command;
    switch (c.type) {
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
    }
    return ack(e.id);
  }

  // ---- Failure injection (the demo's "failure buttons") ----

  /** Cut the internet: stream reconnects by itself when it returns; slides are unaffected. */
  setUplink(uplink: Uplink): void {
    this.set({
      uplink,
      stream: !this.wantStream ? "off" : uplink === "down" ? "reconnecting" : "live",
    });
  }

  /** Kill the engine. It restarts from the last snapshot on the same slide. */
  crash(): void {
    if (this.state.restarting) return;
    this.state = { ...this.state, restarting: true };
    this.emit();
    this.setTimer(() => {
      this.state = { ...this.snapshot, restarting: false, uplink: this.state.uplink };
      this.state.stream = this.wantStream ? (this.state.uplink === "down" ? "reconnecting" : "live") : "off";
      this.emit();
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
