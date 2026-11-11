// The owner's admin page (live.jivvy.org/admin): read the waitlist and church accounts.
//
// Two locks, either of which alone keeps strangers out:
// 1. Cloudflare Access sits in front of /admin and /api/admin/* and makes the visitor sign in
//    with a one-time code Cloudflare emails them.
// 2. Every admin endpoint checks the token Access signs for each request (Cf-Access-Jwt-Assertion):
//    RS256 against the team's published keys, this app's audience, the team as issuer, not expired,
//    and the email in ADMIN_EMAILS. A request that reaches the site any other way (the pages.dev
//    address, a preview, a forged header) gets nothing.
// With ACCESS_TEAM_DOMAIN, ACCESS_AUD or ADMIN_EMAILS unset, the admin endpoints answer 503.
// Nothing here is needed during a service.
import { normalEmail } from "./account";
import { json, type Env } from "./server";

/** The addresses in ADMIN_EMAILS (comma or whitespace separated), normalised. Empty when unset. */
export function adminEmails(list: string | undefined): Set<string> {
  const out = new Set<string>();
  for (const part of (list ?? "").split(/[\s,]+/)) {
    const email = normalEmail(part);
    if (email) out.add(email);
  }
  return out;
}

/** "jivvy" or "https://jivvy.cloudflareaccess.com/" → "https://jivvy.cloudflareaccess.com", or null. */
export function teamOrigin(team: string | undefined): string | null {
  const t = (team ?? "").trim().replace(/\/+$/, "");
  if (/^[a-z0-9-]+$/i.test(t)) return `https://${t.toLowerCase()}.cloudflareaccess.com`;
  return /^https:\/\/[a-z0-9-]+\.cloudflareaccess\.com$/i.test(t) ? t.toLowerCase() : null;
}

type Jwk = JsonWebKey & { kid?: string };
export type FetchKeys = (url: string) => Promise<Jwk[]>;

const defaultFetchKeys: FetchKeys = async (url) => {
  const r = await fetch(url, { cf: { cacheTtl: 300 } } as RequestInit);
  if (!r.ok) throw new Error(`Access keys: ${r.status}`);
  return ((await r.json()) as { keys?: Jwk[] }).keys ?? [];
};

// The team's keys, kept for 10 minutes per isolate. An unknown kid refetches (keys rotate).
const keyCache = new Map<string, { at: number; keys: Jwk[] }>();
const KEYS_TTL_MS = 10 * 60_000;

async function keyFor(certsUrl: string, kid: string, nowMs: number, fetchKeys: FetchKeys): Promise<Jwk | null> {
  const cached = keyCache.get(certsUrl);
  let keys = cached && nowMs - cached.at < KEYS_TTL_MS ? cached.keys : null;
  if (!keys || !keys.some((k) => k.kid === kid)) {
    keys = await fetchKeys(certsUrl);
    keyCache.set(certsUrl, { at: nowMs, keys });
  }
  return keys.find((k) => k.kid === kid && k.kty === "RSA") ?? null;
}

const fromB64url = (s: string) => Uint8Array.from(atob(s.replace(/-/g, "+").replace(/_/g, "/")), (c) => c.charCodeAt(0));

/** The email of a valid Access token for this app, or null. Never throws. */
export async function accessEmail(
  token: string | null,
  opts: { team: string; aud: string; nowMs: number; fetchKeys?: FetchKeys },
): Promise<string | null> {
  try {
    if (!token || token.length > 8192) return null;
    const parts = token.split(".");
    if (parts.length !== 3) return null;
    const [h, p, s] = parts as [string, string, string];
    const header = JSON.parse(new TextDecoder().decode(fromB64url(h))) as { alg?: string; kid?: string };
    if (header.alg !== "RS256" || typeof header.kid !== "string") return null;
    const jwk = await keyFor(`${opts.team}/cdn-cgi/access/certs`, header.kid, opts.nowMs, opts.fetchKeys ?? defaultFetchKeys);
    if (!jwk) return null;
    const key = await crypto.subtle.importKey("jwk", { kty: jwk.kty, n: jwk.n, e: jwk.e }, { name: "RSASSA-PKCS1-v1_5", hash: "SHA-256" }, false, ["verify"]);
    const signed = new TextEncoder().encode(`${h}.${p}`);
    if (!(await crypto.subtle.verify("RSASSA-PKCS1-v1_5", key, fromB64url(s), signed))) return null;
    const c = JSON.parse(new TextDecoder().decode(fromB64url(p))) as { aud?: unknown; iss?: unknown; exp?: unknown; nbf?: unknown; email?: unknown };
    const auds = Array.isArray(c.aud) ? c.aud : [c.aud];
    const now = opts.nowMs / 1000;
    if (!auds.includes(opts.aud) || c.iss !== opts.team) return null;
    if (typeof c.exp !== "number" || c.exp <= now) return null;
    if (typeof c.nbf === "number" && c.nbf > now + 60) return null;
    return normalEmail(c.email);
  } catch {
    return null;
  }
}

/** The signed-in admin's email, or the response to send instead (503 when not set up, 401, 403). */
export async function requireAdmin(request: Request, env: Env, nowMs: number, fetchKeys?: FetchKeys): Promise<string | Response> {
  const admins = adminEmails(env.ADMIN_EMAILS);
  const team = teamOrigin(env.ACCESS_TEAM_DOMAIN);
  const aud = (env.ACCESS_AUD ?? "").trim();
  if (!env.DB || admins.size === 0 || !team || !aud) return json({ ok: false, error: "The admin page isn't set up." }, 503);
  const email = await accessEmail(request.headers.get("cf-access-jwt-assertion"), { team, aud, nowMs, fetchKeys });
  if (!email) return json({ ok: false, error: "Please sign in." }, 401);
  if (!admins.has(email)) return json({ ok: false, error: "This address isn't an admin." }, 403);
  return email;
}

export interface WaitlistRow {
  email: string;
  church: string;
  size: string;
  software: string;
  alphaTester: boolean;
  createdAt: string;
}

const cell = (v: string) => {
  // Quote every cell, and stop a spreadsheet reading a signup's answer as a formula.
  const safe = /^[=+\-@\t\r]/.test(v) ? `'${v}` : v;
  return `"${safe.replace(/"/g, '""')}"`;
};

/** The waitlist as CSV, for a spreadsheet or an email tool. */
export function waitlistCsv(rows: WaitlistRow[]): string {
  const head = ["email", "church", "size", "software", "alpha_tester", "signed_up"];
  const lines = rows.map((r) => [r.email, r.church, r.size, r.software, r.alphaTester ? "yes" : "no", r.createdAt].map(cell).join(","));
  return [head.join(","), ...lines].join("\r\n") + "\r\n";
}
