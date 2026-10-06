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
      "INSERT INTO waitlist (email, church, size, software) VALUES (?1, ?2, ?3, ?4) ON CONFLICT(email) DO UPDATE SET church = ?2, size = ?3, software = ?4",
    ).bind(v.value.email, v.value.church, v.value.size, v.value.software).run();
  } catch {
    return json({ ok: false, error: "Something went wrong on our side. Please try again." }, 500);
  }
  return json({ ok: true });
};
