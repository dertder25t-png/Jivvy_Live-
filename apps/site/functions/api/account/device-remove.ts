// POST /api/account/device-remove { publicKey }: stops one of the signed-in church's computers
// from syncing. Its key never gets a token again, and it no longer counts toward the limit.
import { guardPost, json, readJson, signedIn, type Env } from "../../../src/lib/server";

export const onRequestPost: PagesFunction<Env> = async ({ request, env }) => {
  const blocked = guardPost(request);
  if (blocked) return blocked;
  if (!env.DB) return json({ ok: false, error: "Accounts aren't open yet." }, 503);
  const now = Date.now();
  const church = await signedIn(request, env.DB, now);
  if (!church) return json({ ok: false, error: "Please sign in again." }, 401);
  const key = (await readJson(request)).publicKey;
  if (typeof key !== "string" || key.length > 100) return json({ ok: false, error: "bad request" }, 400);
  const r = await env.DB.prepare("UPDATE sync_devices SET removed_at = ?3 WHERE public_key = ?1 AND church_id = ?2 AND removed_at IS NULL")
    .bind(key, church.id, now)
    .run();
  return r.meta.changes === 1 ? json({ ok: true }) : json({ ok: false, error: "That computer isn't on this account." }, 404);
};
