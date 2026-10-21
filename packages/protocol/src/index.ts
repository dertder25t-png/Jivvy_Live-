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
  | { type: "stream.stop" }
  /** Added in v1 without a version bump: older daemons answer unknown_command. */
  | { type: "state.get" }
  /**
   * Added in v1 without a version bump. After the ack, the daemon sends a `state` event on this
   * connection every time the state changes, until the connection closes.
   */
  | { type: "state.subscribe" }
  /**
   * Added in v1 without a version bump. Audio input levels in dBFS, one entry per channel,
   * sent about ten times a second by the daemon's video process. The daemon relays them to
   * subscribers as `levels` events; they are not part of the saved state.
   */
  | { type: "media.levels"; peakDb: number[]; rmsDb: number[] }
  /**
   * Added in v1 without a version bump. The daemon's video process reports whether the stream
   * it was asked for is actually reaching the platforms, about once a second. UIs read the
   * result as `stream` in the state; they never send this.
   */
  | { type: "stream.report"; status: StreamStatus; destinations?: StreamDestination[] }
  /**
   * Added in v1 without a version bump. Start or stop the local recording. Like the stream,
   * the wish is saved: after a crash the recording resumes into the same set of files.
   * Older daemons answer unknown_command.
   */
  | { type: "recording.start" }
  | { type: "recording.stop" }
  /**
   * Added in v1 without a version bump. The daemon's video process reports whether the
   * recording it was asked for is actually being written, and whether the disk is running
   * low, about once a second. UIs read the result as `recording` and `recordingLowDisk` in
   * the state; they never send this.
   */
  | { type: "recording.report"; status: RecordingStatus; lowDisk?: boolean }
  // ---- Pairing and device authentication (added in v1 without a version bump) ----
  // The church computer's own connection (the local channel) is trusted. Every device on the
  // network channel must be paired, and must prove it holds its key on every connection,
  // before any other command is accepted (`unauthorized` otherwise).
  /** Church computer only. Lets devices ask to pair for PAIRING_WINDOW_MS. */
  | { type: "pair.open" }
  /** Church computer only. Stops accepting pairing requests and drops pending ones. */
  | { type: "pair.close" }
  /**
   * A device asks to pair while pairing is open. `publicKey`: its P-256 public key, raw
   * uncompressed (65 bytes), base64url. The ack carries a code shown on both screens.
   */
  | { type: "pair.request"; publicKey: string; name: string }
  /** Church computer only: approve or deny a pending request after checking the codes match. */
  | { type: "pair.approve"; requestId: string }
  | { type: "pair.deny"; requestId: string }
  /** A device starts proving who it is; the ack carries a one-time challenge. */
  | { type: "auth.hello"; publicKey: string }
  /** ECDSA P-256 / SHA-256 signature (r||s, 64 bytes, base64url) over authMessage(daemonId, challenge). */
  | { type: "auth.prove"; signature: string }
  /**
   * Added in v1 without a version bump. Measures the upload (a few seconds) and answers with a
   * `preflight` event: what each platform will get, e.g. "YouTube 1080p + Facebook 720p".
   * Never while streaming (`unavailable`): the test would take the stream's bandwidth.
   */
  | { type: "stream.preflight" }
  /** Church computer only. */
  | { type: "device.list" }
  /** Church computer only. Removing a device ends its open connections at once. */
  | { type: "device.remove"; deviceId: string }
  /**
   * Added in v1 without a version bump. Replaces the run sheet the screens show; saved on the
   * church computer before the ack, so it survives a crash or restart. A run sheet with another
   * `id` starts again on its first slide; the same `id` (an edit) stays on the current slide.
   * The ack carries the new state (`slideCount`, `runSheetRev`). The whole message must fit in
   * MAX_MESSAGE_BYTES (check with `messageBytes` before sending). Older daemons answer unknown_command.
   */
  | { type: "runsheet.set"; runSheet: RunSheet }
  /** Added in v1 without a version bump. The ack carries `runSheet` (left out when none is set). */
  | { type: "runsheet.get" }
  /**
   * Added in v1 without a version bump. Sends one piece of an image to the church computer,
   * which keeps it under `id`, the SHA-256 of the whole file (64 lowercase hex). Pieces go in
   * order: `offset` is where this one starts (0 starts again), `data` is up to MEDIA_CHUNK_BYTES
   * in base64, and `last` finishes it: the daemon checks the hash and that it is a JPEG, PNG or
   * WebP of at most MAX_MEDIA_BYTES. The ack carries `media`. Slides then show it by `id`.
   */
  | { type: "media.put"; id: string; offset: number; data: string; last: boolean }
  /** Added in v1 without a version bump. Which of these images the church computer lacks (`missing` on the ack). */
  | { type: "media.has"; ids: string[] }
  // ---- The church computer's library (added in v1 without a version bump) ----
  // Songs, plans, themes and settings, kept in a database on the church computer: the master
  // copy during a service, and the only copy without Plus. Each is a document of a `kind`
  // ("song", "plan", "theme", "setting"; newer UIs may add others) with an `id`, a `title` and
  // a `body` the UI shapes; the daemon keeps it as given. Every save bumps `rev`.
  /** The documents of a kind, without their bodies (`docs` on the ack), newest first. */
  | { type: "doc.list"; kind: string }
  /** One document with its body (`doc` on the ack); `bad_arguments` if there is none. */
  | { type: "doc.get"; kind: string; id: string }
  /**
   * Saves a document (`saved` on the ack; subscribers get `doc.changed`). With `baseRev`, only
   * if the saved copy is still that revision (0: only if it doesn't exist yet); otherwise
   * `conflict`, so an edit made on another device is never silently overwritten.
   * The body must be a JSON object of at most MAX_DOC_BYTES.
   */
  | { type: "doc.put"; kind: string; id: string; title: string; body: Record<string, unknown>; baseRev?: number }
  /** Removes a document (with `baseRev`, only that revision); deleting one that's gone is fine. */
  | { type: "doc.delete"; kind: string; id: string; baseRev?: number }
  // ---- Alerts by web push (added in v1 without a version bump) ----
  // The church computer sends its alerts (stream down, no sound, a platform lowered or paused)
  // by web push (RFC 8030/8291, VAPID) to each device that asked, even with no Jivvy screen
  // open. A push carries only the alert's words and a tag. Older daemons answer unknown_command.
  /** The church computer's VAPID public key (`pushKey` on the ack), for `pushManager.subscribe`. */
  | { type: "push.key" }
  /**
   * This device's push subscription, from `PushSubscription.toJSON()`; replaces any it had.
   * Removing the device drops it. `endpoint`: the push service's https URL; `p256dh`: the
   * browser's P-256 key (raw, base64url); `auth`: its 16-byte secret (base64url).
   */
  | { type: "push.subscribe"; endpoint: string; p256dh: string; auth: string }
  /** Pushes a test alert to this device's subscription; `unavailable` if it has none. */
  | { type: "push.test" };

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

export type ErrorCode =
  | "bad_message"
  | "unsupported_version"
  | "unknown_command"
  | "bad_arguments"
  /** The daemon understood the command but can't do it yet (e.g. a feature not built on this daemon). */
  | "unavailable"
  /**
   * Added in v1: this connection may not do that. A device that isn't paired and proven, a
   * command only the church computer may send, or pairing that isn't open.
   */
  | "unauthorized"
  /** Added in v1: doc.put / doc.delete with a `baseRev` that is no longer the saved revision. */
  | "conflict";

export type ParseResult =
  | { ok: true; envelope: Envelope }
  | { ok: false; code: ErrorCode; message: string; id?: string };

const isObj = (x: unknown): x is Record<string, unknown> =>
  typeof x === "object" && x !== null && !Array.isArray(x);

export function isVersionSupported(v: number): boolean {
  return Number.isInteger(v) && v >= MIN_SUPPORTED_VERSION && v <= PROTOCOL_VERSION;
}

/** Most audio channels a levels report may carry. */
export const MAX_CHANNELS = 32;

/** A list of finite dB values, or null. */
function dbList(x: unknown): number[] | null {
  if (!Array.isArray(x) || x.length < 1 || x.length > MAX_CHANNELS) return null;
  return x.every((n) => typeof n === "number" && Number.isFinite(n)) ? (x as number[]) : null;
}

/** Most destinations a stream report may list. */
export const MAX_DESTINATIONS = 16;
const DESTINATION_STATES: readonly DestinationState[] = ["off", "connecting", "live", "reconnecting", "paused"];
/** "1080p", "720p", ..., "audio" (audio only), or "" when not sending. */
const QUALITY = /^(\d{3,4}p|audio)?$/;

/** A stream report's destination list, or null if it isn't one. */
function destinationList(x: unknown): StreamDestination[] | null {
  if (!Array.isArray(x) || x.length > MAX_DESTINATIONS) return null;
  const out: StreamDestination[] = [];
  for (const d of x) {
    if (!isObj(d) || !isId(d.id) || typeof d.name !== "string" || [...d.name].length > MAX_DEVICE_NAME) return null;
    if (!DESTINATION_STATES.includes(d.state as DestinationState)) return null;
    if (typeof d.quality !== "string" || !QUALITY.test(d.quality)) return null;
    if (d.top !== undefined && (typeof d.top !== "string" || !QUALITY.test(d.top))) return null;
    const top = typeof d.top === "string" ? { top: d.top } : {};
    out.push({ id: d.id, name: d.name, state: d.state as DestinationState, quality: d.quality, ...top });
  }
  return out;
}

/** A P-256 public key, raw uncompressed (0x04 || x || y, 65 bytes), base64url without padding. */
const PUBLIC_KEY = /^B[A-Za-z0-9_-]{86}$/;
/** A push service endpoint: an https URL of printable ASCII. */
const PUSH_ENDPOINT = /^https:\/\/[\x21-\x7e]{1,2040}$/;
/** A push subscription's 16-byte auth secret, base64url without padding. */
const PUSH_AUTH = /^[A-Za-z0-9_-]{22}$/;
/** A 64-byte ECDSA signature (r || s), base64url without padding. */
const SIGNATURE = /^[A-Za-z0-9_-]{86}$/;
/** Request and device ids. */
const ID = /^[A-Za-z0-9_-]{1,64}$/;
/** Longest device name, in characters. */
export const MAX_DEVICE_NAME = 64;
/** How long pairing stays open after pair.open. */
export const PAIRING_WINDOW_MS = 120_000;
/** Most pairing requests waiting at once; more are refused until one is answered. */
export const MAX_PENDING_PAIRINGS = 5;

/** A trimmed device name of 1-64 characters with no control characters, or null. */
function deviceName(x: unknown): string | null {
  if (typeof x !== "string") return null;
  const name = x.trim();
  const chars = [...name];
  if (chars.length < 1 || chars.length > MAX_DEVICE_NAME) return null;
  if (chars.some((c) => c.charCodeAt(0) < 0x20 || c.charCodeAt(0) === 0x7f)) return null;
  return name;
}

const isId = (x: unknown): x is string => typeof x === "string" && ID.test(x);

// ---- Run sheets ----

/** Most items in a run sheet. */
export const MAX_RUNSHEET_ITEMS = 200;
/** Most slides in a whole run sheet. */
export const MAX_RUNSHEET_SLIDES = 2000;
/** Most lines on one slide. */
export const MAX_SLIDE_LINES = 16;
/** Longest line on a slide, and longest title, in characters. */
export const MAX_SLIDE_LINE = 200;
export const MAX_TITLE = 200;
/** Longest slide label ("Verse 1", "Chorus"), in characters. */
export const MAX_SLIDE_LABEL = 40;
/** Longest time a looping item waits on one slide. */
export const MAX_ADVANCE_SECONDS = 3600;
/** An item's kind: "song", "scripture", "countdown", "text" today; newer UIs may add others. */
const KIND = /^[a-z][a-z0-9-]{0,31}$/;

/** A string of at most `max` characters with no control characters (tabs included), or null. */
function plainText(x: unknown, max: number): string | null {
  if (typeof x !== "string") return null;
  const chars = [...x];
  if (chars.length > max || chars.some((c) => c.charCodeAt(0) < 0x20 || c.charCodeAt(0) === 0x7f)) return null;
  return x;
}

function parseSlide(x: unknown): Slide | null {
  if (!isObj(x) || !Array.isArray(x.lines) || x.lines.length > MAX_SLIDE_LINES) return null;
  const lines: string[] = [];
  for (const l of x.lines) {
    const line = plainText(l, MAX_SLIDE_LINE);
    if (line === null) return null;
    lines.push(line);
  }
  const slide: Slide = { lines };
  if (x.countdownTo !== undefined) {
    const to = x.countdownTo;
    if (typeof to !== "number" || !Number.isInteger(to) || to < 0) return null;
    slide.countdownTo = to;
  }
  if (x.image !== undefined) {
    if (!isMediaId(x.image)) return null;
    slide.image = x.image;
  }
  if (x.label !== undefined) {
    const label = plainText(x.label, MAX_SLIDE_LABEL);
    if (label === null) return null;
    slide.label = label;
  }
  return slide;
}

// ---- Images ----

/** An image's id: the SHA-256 of its bytes, 64 lowercase hex. */
const MEDIA_ID = /^[0-9a-f]{64}$/;
export const isMediaId = (x: unknown): x is string => typeof x === "string" && MEDIA_ID.test(x);
/** Largest image the church computer keeps. */
export const MAX_MEDIA_BYTES = 20 * 1024 * 1024;
/** Largest piece of an image in one media.put (before base64). */
export const MEDIA_CHUNK_BYTES = 256 * 1024;
const MAX_CHUNK_BASE64 = Math.ceil(MEDIA_CHUNK_BYTES / 3) * 4;
const BASE64 = /^[A-Za-z0-9+/]*={0,2}$/;

/** A valid run sheet with unknown fields dropped, or null. */
export function parseRunSheet(x: unknown): RunSheet | null {
  if (!isObj(x) || !isId(x.id) || !Array.isArray(x.items) || x.items.length > MAX_RUNSHEET_ITEMS) return null;
  const title = plainText(x.title, MAX_TITLE);
  if (title === null) return null;
  const items: RunSheetItem[] = [];
  let total = 0;
  for (const it of x.items) {
    if (!isObj(it) || !isId(it.id) || typeof it.kind !== "string" || !KIND.test(it.kind) || !Array.isArray(it.slides)) return null;
    const itemTitle = plainText(it.title, MAX_TITLE);
    if (itemTitle === null) return null;
    total += it.slides.length;
    if (total > MAX_RUNSHEET_SLIDES) return null;
    const slides: Slide[] = [];
    for (const sl of it.slides) {
      const slide = parseSlide(sl);
      if (!slide) return null;
      slides.push(slide);
    }
    const adv = it.advanceSeconds;
    if (adv === undefined) {
      items.push({ id: it.id, kind: it.kind, title: itemTitle, slides });
      continue;
    }
    if (typeof adv !== "number" || !Number.isInteger(adv) || adv < 1 || adv > MAX_ADVANCE_SECONDS) return null;
    items.push({ id: it.id, kind: it.kind, title: itemTitle, slides, advanceSeconds: adv });
  }
  return { id: x.id, title, items };
}

/**
 * Largest message the daemon takes over WebSocket, in UTF-8 bytes. A run sheet within the limits
 * above can be bigger than this (the limits allow ~6 MB of text); a real service is a few tens of
 * KB. UIs check with `messageBytes` and say so plainly rather than send one that gets refused.
 */
export const MAX_MESSAGE_BYTES = 1024 * 1024;

/** The size of an envelope as sent, in UTF-8 bytes. */
export const messageBytes = (envelope: Envelope): number => jsonBytes(envelope);
const jsonBytes = (x: unknown): number => new TextEncoder().encode(JSON.stringify(x)).length;

// ---- The library ----

/** Largest document body (as JSON, in UTF-8 bytes): a long song or a full plan with room to spare. */
export const MAX_DOC_BYTES = 512 * 1024;
/** A document kind: "song", "plan", "theme", "setting"; newer UIs may add others. */
const DOC_KIND = /^[a-z][a-z0-9-]{0,31}$/;
const isDocKind = (x: unknown): x is string => typeof x === "string" && DOC_KIND.test(x);
const DOC_KIND_HELP = "doc commands need a kind: 1-32 of a-z 0-9 -, starting with a letter";
/** An optional baseRev: undefined when left out, null when it isn't a whole number from 0. */
function parseBaseRev(x: unknown): number | undefined | null {
  if (x === undefined) return undefined;
  return typeof x === "number" && Number.isSafeInteger(x) && x >= 0 ? x : null;
}

/** Every slide of the run sheet in order: what `slideIndex` counts. */
export function runSheetSlides(sheet: RunSheet): { item: RunSheetItem; itemIndex: number; slide: Slide }[] {
  return sheet.items.flatMap((item, itemIndex) => item.slides.map((slide) => ({ item, itemIndex, slide })));
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
    case "recording.start":
    case "recording.stop":
    case "state.get":
    case "state.subscribe":
    case "pair.open":
    case "pair.close":
    case "device.list":
    case "stream.preflight":
    case "runsheet.get":
    case "push.key":
    case "push.test":
      return { ok: true, command: { type: raw.type } };
    case "push.subscribe": {
      const { endpoint, p256dh, auth } = raw;
      if (typeof endpoint !== "string" || !PUSH_ENDPOINT.test(endpoint) || typeof p256dh !== "string"
        || !PUBLIC_KEY.test(p256dh) || typeof auth !== "string" || !PUSH_AUTH.test(auth))
        return { ok: false, code: "bad_arguments", message: "push.subscribe needs an https endpoint, a P-256 p256dh key and a 16-byte auth (base64url)" };
      return { ok: true, command: { type: "push.subscribe", endpoint, p256dh, auth } };
    }
    case "media.put": {
      const ok = isMediaId(raw.id) && typeof raw.offset === "number" && Number.isInteger(raw.offset) && raw.offset >= 0
        && typeof raw.data === "string" && raw.data.length <= MAX_CHUNK_BASE64 && BASE64.test(raw.data);
      if (!ok)
        return { ok: false, code: "bad_arguments", message: `media.put needs an id (SHA-256, hex), an offset and up to ${MEDIA_CHUNK_BYTES} bytes of base64 data` };
      return { ok: true, command: { type: "media.put", id: raw.id as string, offset: raw.offset as number, data: raw.data as string, last: raw.last === true } };
    }
    case "media.has":
      if (!Array.isArray(raw.ids) || raw.ids.length > MAX_RUNSHEET_SLIDES || !raw.ids.every(isMediaId))
        return { ok: false, code: "bad_arguments", message: `media.has needs ids: up to ${MAX_RUNSHEET_SLIDES} SHA-256 ids` };
      return { ok: true, command: { type: "media.has", ids: raw.ids as string[] } };
    case "doc.list":
      if (!isDocKind(raw.kind)) return { ok: false, code: "bad_arguments", message: DOC_KIND_HELP };
      return { ok: true, command: { type: "doc.list", kind: raw.kind } };
    case "doc.get":
    case "doc.delete": {
      const type = raw.type;
      if (!isDocKind(raw.kind) || !isId(raw.id))
        return { ok: false, code: "bad_arguments", message: `${type} needs kind and id (1-64 of A-Z a-z 0-9 _ -)` };
      if (type === "doc.get") return { ok: true, command: { type, kind: raw.kind, id: raw.id } };
      const baseRev = parseBaseRev(raw.baseRev);
      if (baseRev === null) return { ok: false, code: "bad_arguments", message: "baseRev must be a whole number from 0" };
      return { ok: true, command: { type, kind: raw.kind, id: raw.id, ...(baseRev === undefined ? {} : { baseRev }) } };
    }
    case "doc.put": {
      const title = plainText(raw.title, MAX_TITLE);
      const baseRev = parseBaseRev(raw.baseRev);
      if (!isDocKind(raw.kind) || !isId(raw.id) || title === null || !isObj(raw.body) || baseRev === null)
        return { ok: false, code: "bad_arguments", message: `doc.put needs kind, id, title (up to ${MAX_TITLE} characters), a body object and an optional baseRev` };
      if (jsonBytes(raw.body) > MAX_DOC_BYTES)
        return { ok: false, code: "bad_arguments", message: `a document's body can be at most ${MAX_DOC_BYTES / 1024} KB` };
      return {
        ok: true,
        command: { type: "doc.put", kind: raw.kind, id: raw.id, title, body: raw.body, ...(baseRev === undefined ? {} : { baseRev }) },
      };
    }
    case "runsheet.set": {
      const runSheet = parseRunSheet(raw.runSheet);
      if (!runSheet)
        return { ok: false, code: "bad_arguments", message: `runsheet.set needs a runSheet: { id, title, items: up to ${MAX_RUNSHEET_ITEMS} of { id, kind, title, slides } }, up to ${MAX_RUNSHEET_SLIDES} slides of up to ${MAX_SLIDE_LINES} lines` };
      return { ok: true, command: { type: "runsheet.set", runSheet } };
    }
    case "pair.request": {
      const name = deviceName(raw.name);
      if (typeof raw.publicKey !== "string" || !PUBLIC_KEY.test(raw.publicKey) || !name)
        return { ok: false, code: "bad_arguments", message: `pair.request needs a P-256 publicKey (raw, base64url) and a name of 1-${MAX_DEVICE_NAME} characters` };
      return { ok: true, command: { type: "pair.request", publicKey: raw.publicKey, name } };
    }
    case "pair.approve":
    case "pair.deny":
      if (!isId(raw.requestId)) return { ok: false, code: "bad_arguments", message: `${raw.type} needs a requestId` };
      return { ok: true, command: { type: raw.type, requestId: raw.requestId } };
    case "auth.hello":
      if (typeof raw.publicKey !== "string" || !PUBLIC_KEY.test(raw.publicKey))
        return { ok: false, code: "bad_arguments", message: "auth.hello needs a P-256 publicKey (raw, base64url)" };
      return { ok: true, command: { type: "auth.hello", publicKey: raw.publicKey } };
    case "auth.prove":
      if (typeof raw.signature !== "string" || !SIGNATURE.test(raw.signature))
        return { ok: false, code: "bad_arguments", message: "auth.prove needs a 64-byte signature (base64url)" };
      return { ok: true, command: { type: "auth.prove", signature: raw.signature } };
    case "device.remove":
      if (!isId(raw.deviceId)) return { ok: false, code: "bad_arguments", message: "device.remove needs a deviceId" };
      return { ok: true, command: { type: "device.remove", deviceId: raw.deviceId } };
    case "slide.goto":
      if (typeof raw.index !== "number" || !Number.isInteger(raw.index) || raw.index < 0)
        return { ok: false, code: "bad_arguments", message: "slide.goto needs a non-negative integer index" };
      return { ok: true, command: { type: "slide.goto", index: raw.index } };
    case "media.levels": {
      const peakDb = dbList(raw.peakDb), rmsDb = dbList(raw.rmsDb);
      if (!peakDb || !rmsDb || peakDb.length !== rmsDb.length)
        return { ok: false, code: "bad_arguments", message: `media.levels needs peakDb and rmsDb: 1-${MAX_CHANNELS} numbers each, same length` };
      return { ok: true, command: { type: "media.levels", peakDb, rmsDb } };
    }
    case "stream.report": {
      if (raw.status !== "off" && raw.status !== "live" && raw.status !== "reconnecting")
        return { ok: false, code: "bad_arguments", message: "stream.report needs status off, live or reconnecting" };
      if (raw.destinations === undefined) return { ok: true, command: { type: "stream.report", status: raw.status } };
      const destinations = destinationList(raw.destinations);
      if (!destinations)
        return { ok: false, code: "bad_arguments", message: `stream.report destinations: up to ${MAX_DESTINATIONS} of { id, name, state, quality }` };
      return { ok: true, command: { type: "stream.report", status: raw.status, destinations } };
    }
    case "recording.report":
      if (raw.status !== "off" && raw.status !== "recording" && raw.status !== "waiting")
        return { ok: false, code: "bad_arguments", message: "recording.report needs status off, recording or waiting" };
      return { ok: true, command: { type: "recording.report", status: raw.status, lowDisk: raw.lowDisk === true } };
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

export type StreamStatus = "off" | "live" | "reconnecting";

export type DestinationState = "off" | "connecting" | "live" | "reconnecting" | "paused";

/** One streaming platform, as the video process reports it. */
export interface StreamDestination {
  id: string;
  name: string;
  /** `paused`: the bandwidth manager stopped it because the upload can't carry it too. */
  state: DestinationState;
  /** What it sends ("1080p", "720p", ...; "audio" for audio only), or "" when not sending. */
  quality: string;
  /**
   * The best it may send: its own top quality in Advanced mode, else the program's. Sending
   * that is never "lowered", even right after the church lowered the top. Older video
   * processes leave it out.
   */
  top?: string;
}

/** `waiting`: a recording was asked for but nothing is being written yet (starting, or resuming after a crash). */
export type RecordingStatus = "off" | "recording" | "waiting";

/** What every screen needs to show the live service. Sent on acks so a UI never has to guess. */
export interface StateSnapshot {
  slideIndex: number;
  black: boolean;
  stream: StreamStatus;
  /** Added in v1: the operator asked for the stream to be on (it may be reconnecting). */
  streamWanted?: boolean;
  /** Added in v1: whether the local recording is being written. Older daemons leave it out. */
  recording?: RecordingStatus;
  /** Added in v1: the operator asked for the recording to be on (it may be resuming). */
  recordingWanted?: boolean;
  /** Added in v1: the disk the recording goes to has under 10 GB free. */
  recordingLowDisk?: boolean;
  /** Added in v1: each platform's state and quality while streaming; left out when there's nothing to show. */
  streamDestinations?: StreamDestination[];
  /** Added in v1: the run sheet on the daemon. These four are left out when none is set. */
  runSheetId?: string;
  /** Goes up by one every time a run sheet is set, so screens know to fetch it again. */
  runSheetRev?: number;
  /** How many slides the run sheet has (`slideIndex` is below it). */
  slideCount?: number;
  /** The slide at `slideIndex`, for screens that just show it. */
  slide?: Slide;
  /**
   * Added in v1: the body of the active theme (library kind "theme", picked by the "setting"
   * document "active-theme"), as the web app documents it (apps/web/src/lib/theme.ts). Left out
   * when none is picked or it can't be read; screens then draw as they always did.
   */
  theme?: Record<string, unknown>;
}

/** One slide as the screens show it. */
export interface Slide {
  /** Lines of text, top to bottom; none for a blank slide. */
  lines: string[];
  /** A countdown slide: counts down to this time (ms since epoch) under its lines. */
  countdownTo?: number;
  /**
   * Added in v1: an image slide, by id (sent first with media.put). Shown filling the screen,
   * letterboxed, with any lines over it. runsheet.set is refused if the church computer lacks it.
   */
  image?: string;
  /**
   * Added in v1: which part of its item this slide is ("Verse 1", "Chorus"), for the operator's
   * slide list. Never shown on the screens or the stream.
   */
  label?: string;
}

/**
 * One item in the service: a song, a scripture reading, a countdown, an announcement. The UI
 * turns each into slides; the daemon only shows slides, so new kinds need no daemon change.
 */
export interface RunSheetItem {
  id: string;
  /** "song", "scripture", "countdown", "text" or "loop"; screens show unknown kinds as text. */
  kind: string;
  title: string;
  slides: Slide[];
  /**
   * Added in v1: while one of this item's slides is showing, the daemon moves to its next slide
   * every this many seconds, and from its last back to its first (the pre-service loop).
   * `slide.next` from any of its slides goes to the first slide after it (with none, to its own
   * last slide, where it stops looping until the slide changes); Back and go-to move as usual. 1 to MAX_ADVANCE_SECONDS; left out for items that wait for the operator.
   */
  advanceSeconds?: number;
}

export interface RunSheet {
  id: string;
  title: string;
  items: RunSheetItem[];
}

/** A device waiting for approval, as the church computer sees it. */
export interface PairRequestInfo {
  requestId: string;
  name: string;
  /** 6 random digits, also shown on the device: approve only if they match. */
  code: string;
}

export interface PairingStatus {
  open: boolean;
  /** While open: how long until it closes by itself. */
  closesInMs?: number;
  requests: PairRequestInfo[];
}

export interface DeviceInfo {
  deviceId: string;
  name: string;
  /** ms since epoch */
  pairedAt: number;
}

/** One platform in the pre-flight summary. */
export interface PreflightDestination {
  id: string;
  name: string;
  /** What it will get ("1080p", ..., "audio"), or "" if it won't fit and would be paused. */
  quality: string;
}

export interface Preflight {
  /** `measuring` while the upload test runs; `done` with a result; `failed` with none at all. */
  status: "measuring" | "done" | "failed";
  /** The upload speed the summary is for, kbps. */
  uploadKbps?: number;
  /** True if measured just now; false if the test couldn't run and this is the last speed measured. */
  measured?: boolean;
  /** When that speed was measured, ms since epoch. */
  measuredAt?: number;
  destinations: PreflightDestination[];
  /** One plain-English line, e.g. "YouTube 1080p + Facebook 720p". */
  summary: string;
  /** Why it failed or fell back, in words a tech lead can act on. */
  message?: string;
  /**
   * Added in v1: the church computer's other checks (disk, streaming settings, video check,
   * screens, camera and microphone). Left out while measuring, when the test was stopped, and
   * by older daemons (fixtures/preflight-event.json).
   */
  checks?: PreflightCheck[];
  /**
   * Added in v1: the stream started during the test, which stopped it (`failed`, no checks).
   * Nothing is wrong; run it again after the stream. Older daemons leave it out.
   */
  stopped?: true;
}

/** `ok` green, `warn` amber (worth a look), `bad` red (fix before the service). */
export type PreflightLevel = "ok" | "warn" | "bad";

/** One pre-flight check, in plain English, with a fix when it isn't `ok`. */
export interface PreflightCheck {
  /** "disk", "stream", "encoder", "screens", "camera", "microphone"; newer daemons may add others. */
  id: string;
  level: PreflightLevel;
  label: string;
  message: string;
  fix?: string;
}

/**
 * The pre-flight line for these platforms. Shared by both daemons, so they say exactly the same
 * thing (fixtures/preflight-summary.json).
 */
export function preflightSummary(destinations: PreflightDestination[]): string {
  if (destinations.length === 0) return "No streaming platforms are set up.";
  const name = (d: PreflightDestination) => d.name || d.id;
  const fits = destinations.filter((d) => d.quality).map((d) => `${name(d)} ${d.quality === "audio" ? "audio only" : d.quality}`);
  const paused = destinations.filter((d) => !d.quality).map(name);
  const pausedLine = paused.length ? `${paused.join(", ")} won't fit and would be paused.` : "";
  if (!fits.length) return pausedLine;
  return paused.length ? `${fits.join(" + ")}. ${pausedLine}` : fits.join(" + ");
}

/** Fields an ok ack may carry besides `state`; each was added in v1 and only some commands send them. */
export interface AckExtras {
  /** pair.open, pair.close, pair.approve, pair.deny */
  pairing?: PairingStatus;
  /** pair.request: the id the church computer approves, and the code this device shows. */
  request?: { requestId: string; code: string };
  /** auth.hello: sign authMessage(daemonId, challenge) and send it with auth.prove. */
  challenge?: string;
  daemonId?: string;
  /** auth.prove */
  deviceId?: string;
  /** device.list, device.remove */
  devices?: DeviceInfo[];
  /** stream.preflight: `measuring`; the result follows as a `preflight` event. */
  preflight?: Preflight;
  /** runsheet.get: the run sheet, if one is set. */
  runSheet?: RunSheet;
  /** media.put: how much of the image the church computer has, and whether it is complete. */
  media?: { id: string; received: number; complete: boolean };
  /** media.has: the ids the church computer doesn't have yet. */
  missing?: string[];
  /** doc.list */
  docs?: DocInfo[];
  /** doc.get */
  doc?: Doc;
  /** doc.put, doc.delete: the document as saved (a delete is a revision too, so revisions never repeat). */
  saved?: DocInfo;
  /** push.key: the church computer's VAPID public key (raw P-256, base64url). */
  pushKey?: string;
}

/** A document in the church computer's library, without its body. */
export interface DocInfo {
  kind: string;
  id: string;
  title: string;
  /** Goes up by one on every save. */
  rev: number;
  /** When it was last saved, ms since epoch. */
  updatedAt: number;
}

export interface Doc extends DocInfo {
  body: Record<string, unknown>;
}

export type Ack =
  /** `state` was added in v1 without a version bump; older daemons leave it out. */
  | ({ v: number; id: string; ok: true; state?: StateSnapshot } & AckExtras)
  | { v: number; id: string; ok: false; code: ErrorCode; message: string };

/**
 * Unsolicited message from the daemon (no `id`), sent only to connections that asked for it.
 * Clients must ignore events they don't recognise.
 */
export interface Levels {
  peakDb: number[];
  rmsDb: number[];
}

export type DaemonEvent =
  | { v: number; event: "state"; state: StateSnapshot }
  /** Live audio input levels, about ten a second, for meters. */
  | { v: number; event: "levels"; levels: Levels }
  /** To the church computer's subscribers whenever pairing opens, closes or gets a request. */
  | { v: number; event: "pairing"; pairing: PairingStatus }
  /** The pre-flight streaming summary, when the upload test finishes. */
  | { v: number; event: "preflight"; preflight: Preflight }
  /** Something the tech lead should know about, in plain words (e.g. a platform lowered or paused). */
  | { v: number; event: "alert"; alert: Alert }
  /** To the device that asked to pair, once the church computer answers. */
  | { v: number; event: "pair.result"; approved: boolean; deviceId?: string; daemonId?: string }
  /** Added in v1: a document in the library was saved or deleted, by any device. */
  | { v: number; event: "doc.changed"; doc: DocInfo; deleted?: boolean };

/**
 * `stream.lowered`: a platform gets a lower quality because the upload is slow.
 * `stream.paused`: a platform stopped because the upload can't carry it too.
 * `stream.resumed`: a paused platform is streaming again.
 * Added in v1: `stream.down` / `stream.up` (the stream isn't reaching the platforms, and is
 * again) and `audio.silent` / `audio.back` (no sound while streaming, and sound again). These
 * are about the whole stream, so `destinationId`, `name` and `quality` are "". Screens show an
 * alert of a kind they don't know by its `message`.
 */
export type AlertKind = "stream.lowered" | "stream.paused" | "stream.resumed" | ServiceAlertKind;
export type ServiceAlertKind = "stream.down" | "stream.up" | "audio.silent" | "audio.back";

/** How long the stream is down, or silent, before it is an alert. */
export const ALERT_AFTER_MS = 10_000;
/** Below this peak level (dBFS) on every channel, the stream counts as silent. */
export const SILENCE_DB = -60;

/** The words of the whole-stream alerts. Shared by both daemons (fixtures/service-alerts.json). */
export const SERVICE_ALERT_MESSAGES: Record<ServiceAlertKind, string> = {
  "stream.down": "The stream is down: it isn't reaching the platforms. Jivvy keeps trying by itself; check the church computer's internet.",
  "stream.up": "The stream is live again.",
  "audio.silent": "No sound on the stream: check the microphones and the sound desk.",
  "audio.back": "Sound is back on the stream.",
};

export const serviceAlert = (kind: ServiceAlertKind): Alert =>
  ({ kind, destinationId: "", name: "", quality: "", message: SERVICE_ALERT_MESSAGES[kind] });

/** What the "Send test alert" button pushes, with the tag "test". */
export const TEST_ALERT_MESSAGE = "Test alert from Jivvy Live: alerts reach this device.";

/** A web push's payload (JSON, UTF-8): only the alert's words and a tag (its kind, or "test"). */
export interface PushPayload {
  message: string;
  tag: string;
}

export interface Alert {
  kind: AlertKind;
  destinationId: string;
  /** The platform's name as the church set it up. */
  name: string;
  /** What it sends now; "" when paused. */
  quality: string;
  /** One plain-English sentence for the screen and the tech lead. */
  message: string;
}

/** Height of a quality label ("720p" -> 720); audio only ranks below every picture; 0 for none. */
const height = (q: string): number => (q === "audio" ? 1 : Number.parseInt(q, 10) || 0);

/** A quality as people say it. */
const spoken = (q: string): string => (q === "audio" ? "audio only" : q);

const label = (d: StreamDestination) => d.name || d.id;

/**
 * The alerts between two reports of the same stream. Shared by both daemons, so they say
 * exactly the same thing. Only platforms in both reports count: one starting, connecting or
 * reconnecting isn't the bandwidth manager's doing.
 */
export function streamAlerts(before: StreamDestination[], after: StreamDestination[]): Alert[] {
  const out: Alert[] = [];
  for (const now of after) {
    const was = before.find((d) => d.id === now.id);
    if (!was) continue;
    const base = { destinationId: now.id, name: label(now), quality: now.state === "paused" ? "" : now.quality };
    if (now.state === "paused" && was.state !== "paused") {
      out.push({ ...base, kind: "stream.paused",
        message: `${label(now)} is paused: the internet upload can't carry it right now. It comes back by itself when the upload recovers.` });
    } else if (was.state === "paused" && now.state !== "paused" && now.quality) {
      const how = now.quality === "audio" ? "audio only" : `at ${now.quality}`;
      out.push({ ...base, kind: "stream.resumed", message: `${label(now)} is streaming again, ${how}.` });
    } else if (
      was.state !== "paused" && now.state !== "paused" && height(now.quality) > 0 && height(now.quality) < height(was.quality)
      // At its own top quality: the church lowered the top, the upload didn't.
      && now.quality !== now.top
    ) {
      out.push({ ...base, kind: "stream.lowered",
        message: `${label(now)} lowered to ${spoken(now.quality)}: the internet upload is slow. It goes back up by itself.` });
    }
  }
  return out;
}

/**
 * What a device signs to prove it holds its key (UTF-8). The daemon id stops a signature
 * made for one church computer from working on another.
 */
export const authMessage = (daemonId: string, challenge: string): string =>
  `jivvy-live auth v1\n${daemonId}\n${challenge}`;

export const stateEvent = (state: StateSnapshot): DaemonEvent => ({ v: PROTOCOL_VERSION, event: "state", state });
export const alertEvent = (alert: Alert): DaemonEvent => ({ v: PROTOCOL_VERSION, event: "alert", alert });
export const levelsEvent = (levels: Levels): DaemonEvent => ({ v: PROTOCOL_VERSION, event: "levels", levels });

export const ack = (id: string, state?: StateSnapshot, extras: AckExtras = {}): Ack =>
  state ? { v: PROTOCOL_VERSION, id, ok: true, state, ...extras } : { v: PROTOCOL_VERSION, id, ok: true, ...extras };
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
