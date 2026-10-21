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
  LICENSE_PRIVATE_KEY?: string;
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
