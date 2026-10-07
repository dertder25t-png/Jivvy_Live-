import { validateWaitlist } from "../../src/lib/waitlist";

interface Env {
  DB?: D1Database;
}

const json = (body: unknown, status = 200) =>
  new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json", "cache-control": "no-store" } });

export const onRequestPost: PagesFunction<Env> = async ({ request, env }) => {
  let data: unknown;
  try {
    const type = request.headers.get("content-type") ?? "";
    data = type.includes("application/json") ? await request.json() : Object.fromEntries(await request.formData());
  } catch {
    return json({ ok: false, error: "Could not read the form." }, 400);
  }
  const v = validateWaitlist(data);
  if (!v.ok) return json({ ok: false, error: v.error }, 400);
  // Honeypot filled: pretend success so bots learn nothing.
  if (v.spam) return json({ ok: true });
  if (!env.DB) return json({ ok: false, error: "Signups are not open yet. Please email jivvysystems@gmail.com." }, 503);
  try {
    await env.DB.prepare(
      // Signing up again only adds information: blank fields keep the earlier answers,
      // and an unticked box never removes someone from the alpha tester list.
      "INSERT INTO waitlist (email, church, size, software, alpha_tester) VALUES (?1, ?2, ?3, ?4, ?5) " +
        "ON CONFLICT(email) DO UPDATE SET " +
        "church = CASE WHEN ?2 <> '' THEN ?2 ELSE church END, " +
        "size = CASE WHEN ?3 <> '' THEN ?3 ELSE size END, " +
        "software = CASE WHEN ?4 <> '' THEN ?4 ELSE software END, " +
        "alpha_tester = MAX(alpha_tester, ?5)",
    )
      .bind(v.value.email, v.value.church, v.value.size, v.value.software, v.value.alphaTester ? 1 : 0)
      .run();
  } catch {
    return json({ ok: false, error: "Something went wrong on our side. Please try again." }, 500);
  }
  return json({ ok: true });
};
