/**
 * License keys: an Ed25519 signature over what the church bought, checked fully offline.
 *
 * The site (live.jivvy.org) holds the private key and issues keys; the church computer embeds
 * only the public key. A key never stops a service: an expired or missing one only shows a calm
 * notice outside service times. Shared with the Rust daemon (fixtures/license.json).
 *
 * Format: `JL1.<payload>.<signature>`, both base64url without padding. `payload` is the JSON of
 * a LicensePayload; the signature is over the UTF-8 of `LICENSE_SIGNING_PREFIX + payload`
 * (the base64url text, exactly as in the key).
 */

export interface LicensePayload {
  /** The church account's id on live.jivvy.org. */
  church: string;
  /** "church" (the one-time license), "hardship"; newer sites may add others. */
  plan: string;
  /** Local features released before this time are unlocked forever (ms since epoch). */
  updatesUntil: number;
  /** Cloud features (Plus) work until this time (ms since epoch). */
  plusUntil: number;
  /** When the key was issued (ms since epoch). A newer key for the same church replaces an older one. */
  issued: number;
}

export const LICENSE_PREFIX = "JL1.";
export const LICENSE_SIGNING_PREFIX = "jivvy-live license v1\n";
/** Longest license key the church computer takes, in characters. */
export const MAX_LICENSE_KEY = 1024;
/** How long the trial the church computer starts on first run lasts. */
export const TRIAL_MS = 30 * 24 * 60 * 60 * 1000;

const KEY = /^JL1\.[A-Za-z0-9_-]{1,900}\.[A-Za-z0-9_-]{86}$/;
const CHURCH = /^[A-Za-z0-9_-]{1,64}$/;
const PLAN = /^[a-z][a-z0-9-]{0,31}$/;
const isTime = (x: unknown): x is number => typeof x === "number" && Number.isSafeInteger(x) && x >= 0;

/** Looks like a license key (the daemon still checks the signature). Spaces around it are trimmed first. */
export const isLicenseKeyShape = (x: unknown): x is string =>
  typeof x === "string" && x.length <= MAX_LICENSE_KEY && KEY.test(x);

function b64url(bytes: Uint8Array): string {
  let s = "";
  for (const b of bytes) s += String.fromCharCode(b);
  return btoa(s).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

function unb64url(s: string): Uint8Array {
  const bin = atob(s.replace(/-/g, "+").replace(/_/g, "/") + "=".repeat((4 - (s.length % 4)) % 4));
  return Uint8Array.from(bin, (c) => c.charCodeAt(0));
}

/** The payload of a well-formed key, without checking the signature; null if it isn't one. */
export function decodeLicensePayload(key: string): LicensePayload | null {
  if (!isLicenseKeyShape(key)) return null;
  try {
    const raw: unknown = JSON.parse(new TextDecoder().decode(unb64url(key.split(".")[1]!)));
    if (typeof raw !== "object" || raw === null) return null;
    const p = raw as Record<string, unknown>;
    if (typeof p.church !== "string" || !CHURCH.test(p.church) || typeof p.plan !== "string" || !PLAN.test(p.plan)) return null;
    if (!isTime(p.updatesUntil) || !isTime(p.plusUntil) || !isTime(p.issued)) return null;
    return { church: p.church, plan: p.plan, updatesUntil: p.updatesUntil, plusUntil: p.plusUntil, issued: p.issued };
  } catch {
    return null;
  }
}

const signed = (payload: string) => new TextEncoder().encode(LICENSE_SIGNING_PREFIX + payload);
const raw = (b: Uint8Array) => b as Uint8Array<ArrayBuffer>;

/** Issues a key. `privateKey`: an Ed25519 PKCS#8 private key, base64url (the site's secret). */
export async function signLicenseKey(payload: LicensePayload, privateKeyPkcs8: string): Promise<string> {
  const key = await crypto.subtle.importKey("pkcs8", raw(unb64url(privateKeyPkcs8)), { name: "Ed25519" }, false, ["sign"]);
  const { church, plan, updatesUntil, plusUntil, issued } = payload;
  const body = b64url(new TextEncoder().encode(JSON.stringify({ church, plan, updatesUntil, plusUntil, issued })));
  const sig = new Uint8Array(await crypto.subtle.sign({ name: "Ed25519" }, key, signed(body)));
  return `${LICENSE_PREFIX}${body}.${b64url(sig)}`;
}

/** The key's payload if the signature is good for this public key (raw 32 bytes, base64url), else null. */
export async function verifyLicenseKey(key: string, publicKey: string): Promise<LicensePayload | null> {
  const payload = decodeLicensePayload(key);
  if (!payload) return null;
  const [, body, sig] = key.split(".") as [string, string, string];
  try {
    const pub = await crypto.subtle.importKey("raw", raw(unb64url(publicKey)), { name: "Ed25519" }, false, ["verify"]);
    return (await crypto.subtle.verify({ name: "Ed25519" }, pub, raw(unb64url(sig)), signed(body))) ? payload : null;
  } catch {
    return null;
  }
}

/** `trial`: no key yet, inside the 30 days. `trial_ended`: no key, after them. `licensed`: a valid key. */
export type LicenseState = "trial" | "trial_ended" | "licensed";

/**
 * What the church computer says about its license (license.get, license.set). Both commands
 * are the church computer's alone (its local channel or its own browser; `unauthorized` from
 * any other device). A key
 * issued before the saved one is refused (`bad_arguments`); a build with no license public
 * key answers license.set with `unavailable`.
 */
export interface LicenseStatus {
  state: LicenseState;
  /** When this computer's trial started and ends (ms since epoch). */
  trialStartedAt: number;
  trialEndsAt: number;
  /**
   * @deprecated Never filled: the key itself never leaves the church computer. Kept so the
   * type stays add-only; use `keyPresent` and `keyHint`.
   */
  key?: string;
  /** A key is saved. Older daemons leave it out. */
  keyPresent?: boolean;
  /** "JL1…" and the saved key's last 4 characters, to tell keys apart. */
  keyHint?: string;
  /** What the saved key says; left out without one. */
  license?: LicensePayload;
  /** New local features keep arriving (before `updatesUntil`). */
  updatesActive?: boolean;
  /** Plus (the cloud features) is on (before `plusUntil`). */
  plusActive?: boolean;
  /**
   * One calm sentence for the tech lead (trial ending or ended, Plus ended). Only outside a
   * service: never during one, never on the screens or the stream.
   */
  notice?: string;
}
