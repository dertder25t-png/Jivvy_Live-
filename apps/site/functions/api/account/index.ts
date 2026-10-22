// GET /api/account: the signed-in church and its license keys.
import { json, signedIn, type Env } from "../../../src/lib/server";

export const onRequestGet: PagesFunction<Env> = async ({ request, env }) => {
  if (!env.DB) return json({ ok: false, error: "Accounts aren't open yet." }, 503);
  const church = await signedIn(request, env.DB, Date.now());
  if (!church) return json({ ok: false, signedIn: false }, 401);
  const { results } = await env.DB.prepare(
    "SELECT license_key AS key, plan, updates_until AS updatesUntil, plus_until AS plusUntil, issued FROM licenses WHERE church_id = ?1 ORDER BY issued DESC",
  )
    .bind(church.id)
    .all();
  // Church computers that sync; removed ones are left out. Their table arrives with migration
  // 0005, so a database without it still shows the account.
  const devices = await env.DB.prepare(
    "SELECT public_key AS publicKey, name, created_at AS createdAt, last_used_at AS lastUsedAt FROM sync_devices WHERE church_id = ?1 AND removed_at IS NULL ORDER BY created_at",
  )
    .bind(church.id)
    .all()
    .then((r) => r.results)
    .catch(() => []);
  return json({ ok: true, signedIn: true, email: church.email, churchId: church.id, licenses: results, devices });
};
