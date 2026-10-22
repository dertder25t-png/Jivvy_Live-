// The sync token exchange: a church computer proves it holds a registered device key (and, the
// first time, the church's license key) and gets a short-lived Supabase JWT for its church's
// cloud copy (apps/site/supabase/0001_sync.sql reads app_metadata.church_id from it).
//
// 1. POST /api/sync/nonce          -> { nonce }   (single use, 5 minutes)
// 2. POST /api/sync/token { nonce, publicKey, signature, licenseKey?, name? }
//    signature: ECDSA P-256 / SHA-256, raw r||s, base64url, over SYNC_SIGNING_PREFIX + nonce + "\n" + publicKey.
//    publicKey: the uncompressed P-256 point, base64url. A key seen before needs no license key;
//    a new one is registered to the license key's church (at most MAX_DEVICES per church, Plus
//    only). A key the church removed on /account never gets a token again.
//    -> { token, expiresAt, supabaseUrl, supabaseAnonKey }, only while the church has Plus.
// Nothing here is needed during a service: sync pauses then, and the library is on the computer.

/** What the device signs, before the nonce (the daemon's `sync_cloud::SIGNING_PREFIX`). */
export const SYNC_SIGNING_PREFIX = "jivvy-live sync token v1\n";
/** How long a nonce works. */
export const NONCE_TTL_MS = 5 * 60_000;
/** How long a Supabase token works. The church computer asks for a new one before it ends. */
export const TOKEN_TTL_S = 60 * 60;
/** Church computers registered per church. */
export const MAX_DEVICES = 5;

export interface SyncEnv {
  SUPABASE_URL?: string;
  SUPABASE_ANON_KEY?: string;
  SUPABASE_JWT_SECRET?: string;
}

/** Where nonces, devices and licenses live (D1 in production). */
export interface SyncStore {
  /** Deletes the nonce if it exists and hasn't expired; true only for the one caller that did. */
  useNonce(nonce: string, nowMs: number): Promise<boolean>;
  /** A device key seen before: its church, and whether the church removed it. */
  device(publicKey: string): Promise<{ churchId: string; removed: boolean } | null>;
  /** The church a license key was issued to (from the licenses table, never from the key's own text). */
  licenseChurch(licenseKey: string): Promise<string | null>;
  /**
   * Registers a new device key, in one conditional write: only while the church has fewer than
   * `max` devices not removed, so parallel requests can't pass the limit. True if it was added.
   */
  addDevice(publicKey: string, churchId: string, name: string, nowMs: number, max: number): Promise<boolean>;
  /** Notes when the device last got a token (shown on the account page). */
  touchDevice(publicKey: string, nowMs: number): Promise<void>;
  /** The latest plus_until of the church's licenses (0: none). */
  plusUntil(churchId: string): Promise<number>;
}

export type Answer = { status: number; body: Record<string, unknown> };

const b64url = (bytes: Uint8Array) =>
  btoa(String.fromCharCode(...bytes)).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");

function fromB64url(s: string): Uint8Array<ArrayBuffer> | null {
  if (!/^[A-Za-z0-9_-]*$/.test(s)) return null;
  try {
    const bin = atob(s.replace(/-/g, "+").replace(/_/g, "/") + "=".repeat((4 - (s.length % 4)) % 4));
    return Uint8Array.from(bin, (c) => c.charCodeAt(0));
  } catch {
    return null;
  }
}

/** A fresh nonce (32 random bytes, base64url). */
export const newNonce = () => b64url(crypto.getRandomValues(new Uint8Array(32)));

/** The text a device signs for `nonce`. */
export const signedText = (nonce: string, publicKey: string) => `${SYNC_SIGNING_PREFIX}${nonce}\n${publicKey}`;

/** ECDSA P-256 / SHA-256 over `message`, r||s; false for a bad key or signature. */
export async function verifyDevice(publicKey: string, message: string, signature: string): Promise<boolean> {
  const key = fromB64url(publicKey);
  const sig = fromB64url(signature);
  if (!key || key.length !== 65 || key[0] !== 4 || !sig || sig.length !== 64) return false;
  try {
    const k = await crypto.subtle.importKey("raw", key, { name: "ECDSA", namedCurve: "P-256" }, false, ["verify"]);
    return await crypto.subtle.verify({ name: "ECDSA", hash: "SHA-256" }, k, sig, new TextEncoder().encode(message));
  } catch {
    return false;
  }
}

/** An HS256 JWT signed with `secret` (Supabase's JWT secret). */
export async function signJwt(claims: Record<string, unknown>, secret: string): Promise<string> {
  const enc = (o: unknown) => b64url(new TextEncoder().encode(JSON.stringify(o)));
  const head = `${enc({ alg: "HS256", typ: "JWT" })}.${enc(claims)}`;
  const key = await crypto.subtle.importKey("raw", new TextEncoder().encode(secret), { name: "HMAC", hash: "SHA-256" }, false, ["sign"]);
  return `${head}.${b64url(new Uint8Array(await crypto.subtle.sign("HMAC", key, new TextEncoder().encode(head))))}`;
}

/** The claims a church computer's token carries: role and church only, for RLS. */
export function syncClaims(churchId: string, nowMs: number): Record<string, unknown> {
  const iat = Math.floor(nowMs / 1000);
  return {
    iss: "live.jivvy.org",
    aud: "authenticated",
    role: "authenticated",
    iat,
    exp: iat + TOKEN_TTL_S,
    app_metadata: { church_id: churchId },
  };
}

const str = (x: unknown, max: number) => (typeof x === "string" && x.length > 0 && x.length <= max ? x : null);

/** Answers POST /api/sync/token. Never logs or returns a key, a signature or the token's secret. */
export async function handleSyncToken(data: Record<string, unknown>, env: SyncEnv, store: SyncStore, nowMs: number): Promise<Answer> {
  if (!env.SUPABASE_URL || !env.SUPABASE_ANON_KEY || !env.SUPABASE_JWT_SECRET) return { status: 503, body: { ok: false, error: "Sync isn't open yet." } };
  const nonce = str(data.nonce, 64);
  const publicKey = str(data.publicKey, 100);
  const signature = str(data.signature, 100);
  const licenseKey = data.licenseKey === undefined ? null : str(data.licenseKey, 2048);
  if (!nonce || !publicKey || !signature) return { status: 400, body: { ok: false, error: "bad request" } };
  // The signature first (no state touched by a forgery), then the nonce, used once either way.
  if (!(await verifyDevice(publicKey, signedText(nonce, publicKey), signature)))
    return { status: 401, body: { ok: false, error: "bad signature" } };
  if (!(await store.useNonce(nonce, nowMs))) return { status: 401, body: { ok: false, error: "nonce expired; ask for a new one" } };
  const noPlus: Answer = { status: 403, body: { ok: false, error: "no_plus" } };
  const known = await store.device(publicKey);
  // Removed on the account page: this key never gets a token again.
  if (known?.removed) return { status: 403, body: { ok: false, error: "device_removed" } };
  let church = known?.churchId ?? null;
  if (church) {
    if ((await store.plusUntil(church)) <= nowMs) return noPlus;
  } else {
    // A new church computer: registered to the church whose license key it holds, only with Plus.
    if (!licenseKey) return { status: 401, body: { ok: false, error: "unknown_device" } };
    church = await store.licenseChurch(licenseKey);
    if (!church) return { status: 403, body: { ok: false, error: "That license key isn't one we issued." } };
    if ((await store.plusUntil(church)) <= nowMs) return noPlus;
    const name = typeof data.name === "string" ? data.name.replace(/[\u0000-\u001f\u007f]/g, "").slice(0, 64) : "";
    if (!(await store.addDevice(publicKey, church, name, nowMs, MAX_DEVICES))) {
      // Lost a race with itself (fine) or the church is at the limit.
      const now = await store.device(publicKey);
      if (!now || now.removed || now.churchId !== church)
        return {
          status: 403,
          body: { ok: false, error: `This church already syncs on ${MAX_DEVICES} computers. Remove one at live.jivvy.org/account.` },
        };
    }
  }
  await store.touchDevice(publicKey, nowMs);
  const claims = syncClaims(church, nowMs);
  return {
    status: 200,
    body: {
      ok: true,
      token: await signJwt(claims, env.SUPABASE_JWT_SECRET),
      expiresAt: (claims.exp as number) * 1000,
      supabaseUrl: env.SUPABASE_URL,
      supabaseAnonKey: env.SUPABASE_ANON_KEY,
    },
  };
}
