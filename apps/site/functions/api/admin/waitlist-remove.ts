// POST /api/admin/waitlist-remove { email }: takes one address off the waitlist (for a request
// to be removed, or an obvious junk signup). It is deleted, not hidden.
import { requireAdmin } from "../../../src/lib/admin";
import { guardPost, json, readJson, type Env } from "../../../src/lib/server";

export const onRequestPost: PagesFunction<Env> = async ({ request, env }) => {
  const blocked = guardPost(request);
  if (blocked) return blocked;
  const admin = await requireAdmin(request, env, Date.now());
  if (admin instanceof Response) return admin;
  const raw = (await readJson(request)).email;
  // Stored trimmed and lowercased by /api/waitlist, so match that text exactly.
  const email = typeof raw === "string" ? raw.trim().toLowerCase() : "";
  if (!email || email.length > 254) return json({ ok: false, error: "bad request" }, 400);
  const r = await env.DB!.prepare("DELETE FROM waitlist WHERE email = ?1").bind(email).run();
  return r.meta.changes === 1 ? json({ ok: true }) : json({ ok: false, error: "That address isn't on the waitlist." }, 404);
};
