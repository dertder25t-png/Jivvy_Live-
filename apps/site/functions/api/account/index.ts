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
  return json({ ok: true, signedIn: true, email: church.email, churchId: church.id, licenses: results });
};
