// GET /api/admin/waitlist: every waitlist signup, newest first. `?format=csv` downloads it.
import { requireAdmin, waitlistCsv, type WaitlistRow } from "../../../src/lib/admin";
import { json, type Env } from "../../../src/lib/server";

export const onRequestGet: PagesFunction<Env> = async ({ request, env }) => {
  const admin = await requireAdmin(request, env, Date.now());
  if (admin instanceof Response) return admin;
  const { results } = await env.DB!.prepare(
    "SELECT email, church, size, software, alpha_tester, created_at FROM waitlist ORDER BY created_at DESC, email",
  ).all<{ email: string; church: string; size: string; software: string; alpha_tester: number; created_at: string }>();
  const rows: WaitlistRow[] = results.map((r) => ({
    email: r.email,
    church: r.church,
    size: r.size,
    software: r.software,
    alphaTester: r.alpha_tester === 1,
    createdAt: r.created_at,
  }));
  if (new URL(request.url).searchParams.get("format") === "csv") {
    return new Response(waitlistCsv(rows), {
      headers: {
        "content-type": "text/csv; charset=utf-8",
        "content-disposition": `attachment; filename="jivvy-waitlist-${new Date().toISOString().slice(0, 10)}.csv"`,
        "cache-control": "no-store",
      },
    });
  }
  return json({ ok: true, email: admin, waitlist: rows });
};
