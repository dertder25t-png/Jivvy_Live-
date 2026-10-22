// Shared by the Pages Functions: bindings, secrets, JSON answers and the signed-in church.
import { SESSION_COOKIE, readCookie, sha256Hex } from "./account";

/** Bindings and secrets (apps/site/README.md lists what the owner sets). All optional, so a missing one answers 503. */
export interface Env {
  DB?: D1Database;
  /** Cloudflare Email Sending binding, for sign-in codes only. */
  EMAIL?: SendEmail;
  /** The address sign-in codes come from, on a domain onboarded to Email Sending. */
  EMAIL_FROM?: string;
  /** e.g. https://live.jivvy.org, for Stripe's return links. */
  SITE_URL?: string;
  STRIPE_SECRET_KEY?: string;
  STRIPE_PRICE_ID?: string;
  STRIPE_WEBHOOK_SECRET?: string;
  /** Production signing key. Not used while Stripe is in test mode. */
  LICENSE_PRIVATE_KEY?: string;
  /** The published test key (packages/protocol/fixtures/license.json); only debug daemons accept licenses signed by it. */
  LICENSE_TEST_PRIVATE_KEY?: string;
  /** Public halves of the two license keys, for checking the signature on alert emails (raw Ed25519, base64url). */
  LICENSE_PUBLIC_KEY?: string;
  LICENSE_TEST_PUBLIC_KEY?: string;
}

export const json = (body: unknown, status = 200, headers: Record<string, string> = {}) =>
  new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json", "cache-control": "no-store", ...headers },
  });

export async function readBody(request: Request): Promise<Record<string, unknown>> {
  try {
    const type = request.headers.get("content-type") ?? "";
    const data: unknown = type.includes("application/json") ? await request.json() : Object.fromEntries(await request.formData());
    return typeof data === "object" && data !== null ? (data as Record<string, unknown>) : {};
  } catch {
    return {};
  }
}

/** Strict JSON body (`guardPost` has already required application/json). */
export async function readJson(request: Request): Promise<Record<string, unknown>> {
  try {
    const data: unknown = await request.json();
    return typeof data === "object" && data !== null && !Array.isArray(data) ? (data as Record<string, unknown>) : {};
  } catch {
    return {};
  }
}

/**
 * CSRF guard for cookie-authenticated POSTs: JSON only (a cross-site form can't send it without
 * a CORS preflight) and the request must come from this site's own origin.
 */
export function guardPost(request: Request): Response | null {
  const type = (request.headers.get("content-type") ?? "").split(";")[0]!.trim().toLowerCase();
  if (type !== "application/json") return json({ ok: false, error: "Unsupported request." }, 415);
  const origin = request.headers.get("origin");
  const own = new URL(request.url).origin;
  const ok = origin !== null ? origin === own : request.headers.get("sec-fetch-site") === "same-origin";
  return ok ? null : json({ ok: false, error: "Forbidden." }, 403);
}

/** The caller's address, hashed so the limiter table holds no raw IPs. */
export async function clientKey(request: Request): Promise<string> {
  return (await sha256Hex(`ip\n${request.headers.get("cf-connecting-ip") ?? "unknown"}`)).slice(0, 32);
}

/**
 * Counts one hit against `bucket` in a fixed window; true while the caller is still within `max`.
 * One atomic upsert, so concurrent requests can't slip past the limit.
 */
export async function withinLimit(db: D1Database, bucket: string, max: number, windowMs: number, nowMs: number): Promise<boolean> {
  const row = await db
    .prepare(
      "INSERT INTO rate_limits (bucket, window_start, hits) VALUES (?1, ?2, 1) " +
        "ON CONFLICT(bucket) DO UPDATE SET " +
        "hits = CASE WHEN window_start <= ?2 - ?3 THEN 1 ELSE hits + 1 END, " +
        "window_start = CASE WHEN window_start <= ?2 - ?3 THEN ?2 ELSE window_start END " +
        "RETURNING hits",
    )
    .bind(bucket, nowMs, windowMs)
    .first<{ hits: number }>();
  return !!row && row.hits <= max;
}

export interface Church {
  id: string;
  email: string;
}

/** The church signed in on this request, if any. */
export async function signedIn(request: Request, db: D1Database, nowMs: number): Promise<Church | null> {
  const token = readCookie(request.headers.get("cookie"), SESSION_COOKIE);
  if (!token) return null;
  const row = await db
    .prepare("SELECT c.id, c.email FROM sessions s JOIN churches c ON c.id = s.church_id WHERE s.token_hash = ?1 AND s.expires_at > ?2")
    .bind(await sha256Hex(token), nowMs)
    .first<Church>();
  return row ?? null;
}
