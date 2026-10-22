// POST /api/alert: the church computer's stream-down and no-sound alerts, emailed to the church
// account's own address. Never the waitlist, never anyone else. Nothing here is needed during a
// service: the church computer sends this in the background and the service never waits for it.
import { ALERT_EMAIL_BACK_OF, ALERT_EMAIL_KINDS, SERVICE_ALERT_MESSAGES, verifyLicenseKey, type ServiceAlertKind } from "@jivvy/protocol";

/** At most one email per church per alert kind in this long. */
export const KIND_WINDOW_MS = 10 * 60_000;
/** At most this many alert emails per church per day. */
export const PER_DAY = 20;

export interface AlertEnv {
  /** The production license public key (raw Ed25519, base64url). */
  LICENSE_PUBLIC_KEY?: string;
  /** The published test key's public half (packages/protocol/fixtures/license.json); test-mode licenses are signed with its private half. */
  LICENSE_TEST_PUBLIC_KEY?: string;
}

/** Where the limits live and where the email goes. */
export interface AlertStore {
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

export async function handleAlert(data: Record<string, unknown>, env: AlertEnv, store: AlertStore, nowMs: number): Promise<Answer> {
  const kind = data.kind;
  const key = data.key;
  if (typeof kind !== "string" || !(ALERT_EMAIL_KINDS as readonly string[]).includes(kind) || typeof key !== "string")
    return { status: 400, body: { ok: false, error: "Unsupported alert." } };
  const publicKeys = [env.LICENSE_PUBLIC_KEY, env.LICENSE_TEST_PUBLIC_KEY].filter((k): k is string => !!k);
  if (publicKeys.length === 0) return { status: 503, body: { ok: false, error: "Alert emails aren't open yet." } };
  let church: string | null = null;
  for (const pub of publicKeys) {
    church = (await verifyLicenseKey(key, pub))?.church ?? null;
    if (church) break;
  }
  const to = church ? await store.churchEmail(church) : null;
  if (!church || !to) return { status: 401, body: { ok: false, error: "Unknown license." } };
  const alertKind = kind as ServiceAlertKind;
  // A "back" is only worth an email if its "down" was emailed (and no "back" has followed since).
  const down = ALERT_EMAIL_BACK_OF[alertKind];
  if (down && (await store.lastSent(church, down)) <= (await store.lastSent(church, alertKind))) return ok(false, "no down email");
  const previous = await store.claim(church, alertKind, nowMs);
  if (previous === null) return ok(false, "throttled");
  if (!(await store.underDailyLimit(church, nowMs))) {
    await store.release(church, alertKind, previous, nowMs);
    return ok(false, "daily limit");
  }
  const mail = alertEmail(alertKind);
  try {
    await store.send(to, mail.subject, mail.text);
  } catch {
    await store.release(church, alertKind, previous, nowMs);
    return { status: 502, body: { ok: false, error: "We couldn't send the email just now." } };
  }
  return ok(true);
}
