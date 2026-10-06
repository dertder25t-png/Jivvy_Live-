/**
 * Jivvy Live command protocol.
 *
 * Rules (see CLAUDE.md #4): add fields, never rename or remove them. The daemon accepts the
 * current and previous version so a slightly older UI keeps working. Unknown commands and
 * unknown extra fields are never fatal: they are rejected / ignored with a typed reason.
 */

export const PROTOCOL_VERSION = 1;
/** Oldest version the daemon still accepts. Bump only when dropping support deliberately. */
export const MIN_SUPPORTED_VERSION = 1;

export type Command =
  | { type: "slide.next" }
  | { type: "slide.prev" }
  | { type: "slide.goto"; index: number }
  | { type: "output.black"; on: boolean }
  | { type: "stream.start" }
  | { type: "stream.stop" };

export type CommandType = Command["type"];

export interface Envelope {
  /** Protocol version the sender speaks. */
  v: number;
  /** Sender-generated id, echoed in the ack so the UI can match replies. */
  id: string;
  /** Sender timestamp, ms since epoch. */
  ts: number;
  command: Command;
}

export type ErrorCode = "bad_message" | "unsupported_version" | "unknown_command" | "bad_arguments";

export type ParseResult =
  | { ok: true; envelope: Envelope }
  | { ok: false; code: ErrorCode; message: string; id?: string };

const isObj = (x: unknown): x is Record<string, unknown> =>
  typeof x === "object" && x !== null && !Array.isArray(x);

export function isVersionSupported(v: number): boolean {
  return Number.isInteger(v) && v >= MIN_SUPPORTED_VERSION && v <= PROTOCOL_VERSION;
}

type CommandResult = { ok: true; command: Command } | { ok: false; code: ErrorCode; message: string };

function parseCommand(raw: unknown): CommandResult {
  if (!isObj(raw) || typeof raw.type !== "string")
    return { ok: false, code: "bad_message", message: "command.type missing" };
  switch (raw.type) {
    case "slide.next":
    case "slide.prev":
    case "stream.start":
    case "stream.stop":
      return { ok: true, command: { type: raw.type } };
    case "slide.goto":
      if (typeof raw.index !== "number" || !Number.isInteger(raw.index) || raw.index < 0)
        return { ok: false, code: "bad_arguments", message: "slide.goto needs a non-negative integer index" };
      return { ok: true, command: { type: "slide.goto", index: raw.index } };
    case "output.black":
      if (typeof raw.on !== "boolean")
        return { ok: false, code: "bad_arguments", message: "output.black needs boolean on" };
      return { ok: true, command: { type: "output.black", on: raw.on } };
    default:
      return { ok: false, code: "unknown_command", message: `unknown command ${raw.type}` };
  }
}

/** Parse and validate an incoming message (string or already-decoded JSON). Never throws. */
export function parseEnvelope(input: unknown): ParseResult {
  let raw = input;
  if (typeof input === "string") {
    try {
      raw = JSON.parse(input);
    } catch {
      return { ok: false, code: "bad_message", message: "invalid JSON" };
    }
  }
  if (!isObj(raw)) return { ok: false, code: "bad_message", message: "message must be an object" };
  const id = typeof raw.id === "string" ? raw.id : undefined;
  if (!id || typeof raw.ts !== "number" || typeof raw.v !== "number")
    return { ok: false, code: "bad_message", message: "v, id and ts are required", id };
  if (!isVersionSupported(raw.v))
    return { ok: false, code: "unsupported_version", message: `version ${raw.v} not supported`, id };
  const cmd = parseCommand(raw.command);
  if (!cmd.ok) return { ...cmd, id };
  return { ok: true, envelope: { v: raw.v, id, ts: raw.ts, command: cmd.command } };
}

export function makeEnvelope(command: Command, opts: { id?: string; now?: number } = {}): Envelope {
  return { v: PROTOCOL_VERSION, id: opts.id ?? crypto.randomUUID(), ts: opts.now ?? Date.now(), command };
}

export type Ack =
  | { v: number; id: string; ok: true }
  | { v: number; id: string; ok: false; code: ErrorCode; message: string };

export const ack = (id: string): Ack => ({ v: PROTOCOL_VERSION, id, ok: true });
export const nack = (id: string, code: ErrorCode, message: string): Ack => ({
  v: PROTOCOL_VERSION, id, ok: false, code, message,
});

/**
 * Handler interface implemented by the real daemon AND the in-browser simulated daemon used by
 * the demo and tests, so the UI never knows which one it talks to.
 */
export interface DaemonLike {
  handle(envelope: Envelope): Ack | Promise<Ack>;
}

/** Decode + dispatch one message to a daemon, always returning an Ack. */
export async function dispatch(daemon: DaemonLike, input: unknown): Promise<Ack> {
  const parsed = parseEnvelope(input);
  if (!parsed.ok) return nack(parsed.id ?? "", parsed.code, parsed.message);
  try {
    return await daemon.handle(parsed.envelope);
  } catch {
    return nack(parsed.envelope.id, "bad_message", "daemon error");
  }
}
