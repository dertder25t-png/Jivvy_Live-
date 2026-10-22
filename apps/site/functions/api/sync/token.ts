// POST /api/sync/token: a church computer's signed nonce for a short-lived Supabase token (src/lib/syncToken.ts).
import { handleSyncToken, type SyncStore } from "../../../src/lib/syncToken";
import { clientKey, json, readJson, withinLimit, type Env } from "../../../src/lib/server";

/** Token requests per caller per hour. */
const PER_HOUR = 60;

export const onRequestPost: PagesFunction<Env> = async ({ request, env }) => {
  if (!env.DB) return json({ ok: false, error: "Sync isn't open yet." }, 503);
  const db = env.DB;
  const now = Date.now();
  if (!(await withinLimit(db, `sync-token:${await clientKey(request)}`, PER_HOUR, 60 * 60_000, now)))
    return json({ ok: false, error: "Too many tries. Please wait a while." }, 429);
  const store: SyncStore = {
    async useNonce(nonce, nowMs) {
      return !!(await db.prepare("DELETE FROM sync_nonces WHERE nonce = ?1 AND expires_at > ?2 RETURNING nonce").bind(nonce, nowMs).first());
    },
    async deviceChurch(publicKey) {
      const row = await db.prepare("SELECT church_id FROM sync_devices WHERE public_key = ?1").bind(publicKey).first<{ church_id: string }>();
      return row?.church_id ?? null;
    },
    async licenseChurch(licenseKey) {
      const row = await db.prepare("SELECT church_id FROM licenses WHERE license_key = ?1").bind(licenseKey).first<{ church_id: string }>();
      return row?.church_id ?? null;
    },
    async deviceCount(churchId) {
      const row = await db.prepare("SELECT COUNT(*) AS n FROM sync_devices WHERE church_id = ?1").bind(churchId).first<{ n: number }>();
      return row?.n ?? 0;
    },
    async addDevice(publicKey, churchId, nowMs) {
      await db
        .prepare("INSERT OR IGNORE INTO sync_devices (public_key, church_id, created_at) VALUES (?1, ?2, ?3)")
        .bind(publicKey, churchId, nowMs)
        .run();
    },
    async plusUntil(churchId) {
      const row = await db.prepare("SELECT MAX(plus_until) AS t FROM licenses WHERE church_id = ?1").bind(churchId).first<{ t: number | null }>();
      return row?.t ?? 0;
    },
  };
  const r = await handleSyncToken(await readJson(request), env, store, now);
  return json(r.body, r.status);
};
