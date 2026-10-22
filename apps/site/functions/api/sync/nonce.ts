// POST /api/sync/nonce: a single-use nonce for a church computer to sign (src/lib/syncToken.ts).
import { NONCE_TTL_MS, newNonce } from "../../../src/lib/syncToken";
import { clientKey, json, withinLimit, type Env } from "../../../src/lib/server";

/** Nonces per caller per hour (a church computer needs about one an hour). */
const PER_HOUR = 60;

export const onRequestPost: PagesFunction<Env> = async ({ request, env }) => {
  if (!env.DB || !env.SUPABASE_JWT_SECRET) return json({ ok: false, error: "Sync isn't open yet." }, 503);
  const now = Date.now();
  if (!(await withinLimit(env.DB, `sync-nonce:${await clientKey(request)}`, PER_HOUR, 60 * 60_000, now)))
    return json({ ok: false, error: "Too many tries. Please wait a while." }, 429);
  const nonce = newNonce();
  // Expired ones go as new ones are made, so the table stays small.
  await env.DB.batch([
    env.DB.prepare("DELETE FROM sync_nonces WHERE expires_at <= ?1").bind(now),
    env.DB.prepare("INSERT INTO sync_nonces (nonce, expires_at) VALUES (?1, ?2)").bind(nonce, now + NONCE_TTL_MS),
  ]);
  return json({ ok: true, nonce });
};
