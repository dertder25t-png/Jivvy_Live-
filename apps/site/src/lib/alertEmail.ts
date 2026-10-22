// POST /api/alert: the church computer's stream-down and no-sound alerts, emailed to the church
// account's own address. Never the waitlist, never anyone else. Nothing here is needed during a
// service: the church computer sends this in the background and the service never waits for it.
import { ALERT_EMAIL_BACK_OF, ALERT_EMAIL_KINDS, SERVICE_ALERT_MESSAGES, decodeLicensePayload, type ServiceAlertKind } from "@jivvy/protocol";
import { sha256Hex } from "./account";

/** At most one email per church per alert kind in this long. */
export const KIND_WINDOW_MS = 10 * 60_000;
/** At most this many alert emails per church per day. */
export const PER_DAY = 20;

/** Longest request body /api/alert reads (a license key is under 1100 characters). */
export const MAX_BODY = 4096;

/** Where the limits live and where the email goes. */
export interface AlertStore {
  /** The license keys this site issued and recorded for the church (the licenses table). */
  issuedKeys(churchId: string): Promise<string[]>;
  /** The church account's own address, if the church exists. */
  churchEmail(churchId: string): Promise<string | null>;
  /** When an email of this kind was last sent to the church (0 if never). */
  lastSent(churchId: string, kind: string): Promise<number>;
  /** Takes the kind's slot atomically if its last email was over `KIND_WINDOW_MS` ago; the previous time, or null if throttled. */
  claim(churchId: string, kind: string, nowMs: number): Promise<number | null>;
  /** Gives the slot back after a failed send, so the retry is not throttled. */
  release(churchId: string, kind: string, previous: number, nowMs: number): Promise<void>;
  /** Counts one more email today; false once the church is over `PER_DAY`. */
  underDailyLimit(churchId: string, nowMs: number): Promise<boolean>;
  /** Gives back the day's count taken by `underDailyLimit` when the email was not sent. */
  refundDaily(churchId: string, nowMs: number): Promise<void>;
  send(to: string, subject: string, text: string): Promise<void>;
}

type Answer = { status: number; body: Record<string, unknown> };
const ok = (sent: boolean, reason?: string): Answer => ({ status: 200, body: { ok: true, sent, ...(reason ? { reason } : {}) } });

/** The email: the alert's words exactly as the screens and pushes say them (the site's own copy, never the caller's text). */
export function alertEmail(kind: ServiceAlertKind): { subject: string; text: string } {
  return {
    subject: "Jivvy Live alert",
    text:
      `${SERVICE_ALERT_MESSAGES[kind]}\n\n` +
      "You get this because alert emails are turned on in Jivvy Live on your church computer. " +
      "You can turn them off there, under Alerts.\n",
  };
}

/** Equal strings, compared without stopping at the first difference. */
function sameHash(a: string, b: string): boolean {
  let diff = a.length ^ b.length;
  for (let i = 0; i < Math.max(a.length, b.length); i++) diff |= (a.charCodeAt(i) || 0) ^ (b.charCodeAt(i) || 0);
  return diff === 0;
}

/**
 * The church the posted key belongs to, only if it is exactly a key this site issued and recorded
 * for that church. A signature alone proves nothing: the test signing key is published, so
 * anyone can sign a "valid" key for any church id. Compared as SHA-256 hashes, in constant time.
 */
async function issuedTo(key: string, store: AlertStore): Promise<string | null> {
  const church = decodeLicensePayload(key)?.church;
  if (!church) return null;
  const posted = await sha256Hex(key);
  let found = false;
  for (const issued of await store.issuedKeys(church)) found = sameHash(posted, await sha256Hex(issued)) || found;
  return found ? church : null;
}

export async function handleAlert(data: Record<string, unknown>, store: AlertStore, nowMs: number): Promise<Answer> {
  const kind = data.kind;
  const key = data.key;
  if (typeof kind !== "string" || !(ALERT_EMAIL_KINDS as readonly string[]).includes(kind) || typeof key !== "string")
    return { status: 400, body: { ok: false, error: "Unsupported alert." } };
  const church = await issuedTo(key, store);
  const to = church ? await store.churchEmail(church) : null;
  if (!church || !to) return { status: 401, body: { ok: false, error: "Unknown license." } };
  const alertKind = kind as ServiceAlertKind;
  // A "back" is only worth an email if its "down" was emailed (and no "back" has followed since).
  const down = ALERT_EMAIL_BACK_OF[alertKind];
  if (down && (await store.lastSent(church, down)) <= (await store.lastSent(church, alertKind))) return ok(false, "no down email");
  const previous = await store.claim(church, alertKind, nowMs);
  if (previous === null) return ok(false, "throttled");
  // Everything after the claim undoes what it took if it doesn't end in a sent email.
  let sent = false;
  let counted = false;
  try {
    if (!(await store.underDailyLimit(church, nowMs))) return ok(false, "daily limit");
    counted = true;
    const mail = alertEmail(alertKind);
    await store.send(to, mail.subject, mail.text);
    sent = true;
    return ok(true);
  } catch {
    return { status: 502, body: { ok: false, error: "We couldn't send the email just now." } };
  } finally {
    if (!sent) {
      await store.release(church, alertKind, previous, nowMs).catch(() => {});
      if (counted) await store.refundDaily(church, nowMs).catch(() => {});
    }
  }
}
