// GET /api/admin/accounts: church accounts with their licenses and syncing computers (counts and
// dates only; license keys are never sent to this page).
import { requireAdmin } from "../../../src/lib/admin";
import { json, type Env } from "../../../src/lib/server";

export const onRequestGet: PagesFunction<Env> = async ({ request, env }) => {
  const now = Date.now();
  const admin = await requireAdmin(request, env, now);
  if (admin instanceof Response) return admin;
  const { results } = await env.DB!.prepare(
    "SELECT c.id, c.email, c.created_at AS createdAt, " +
      "(SELECT COUNT(*) FROM licenses l WHERE l.church_id = c.id) AS licenses, " +
      "(SELECT MAX(plus_until) FROM licenses l WHERE l.church_id = c.id) AS plusUntil " +
      "FROM churches c ORDER BY c.created_at DESC",
  ).all<{ id: string; email: string; createdAt: number; licenses: number; plusUntil: number | null }>();
  // Syncing computers arrive with migration 0005; a database without it still lists accounts.
  const devices = await env.DB!.prepare("SELECT church_id AS id, COUNT(*) AS n FROM sync_devices WHERE removed_at IS NULL GROUP BY church_id")
    .all<{ id: string; n: number }>()
    .then((r) => new Map(r.results.map((d) => [d.id, d.n])))
    .catch(() => new Map<string, number>());
  const accounts = results.map((c) => ({ ...c, plusActive: (c.plusUntil ?? 0) > now, devices: devices.get(c.id) ?? 0 }));
  return json({ ok: true, accounts });
};
