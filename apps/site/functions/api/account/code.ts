// POST /api/account/code { email }: emails a one-time sign-in code to that address only.
import { CODE_TTL_MS, RESEND_MS, codeHash, newCode, normalEmail, signInEmail } from "../../../src/lib/account";
import { json, readBody, type Env } from "../../../src/lib/server";

export const onRequestPost: PagesFunction<Env> = async ({ request, env }) => {
  const email = normalEmail((await readBody(request)).email);
  if (!email) return json({ ok: false, error: "Please enter a valid email address." }, 400);
  if (!env.DB || !env.EMAIL || !env.EMAIL_FROM) return json({ ok: false, error: "Sign-in isn't open yet." }, 503);
  const now = Date.now();
  const last = await env.DB.prepare("SELECT sent_at FROM sign_in_codes WHERE email = ?1").bind(email).first<{ sent_at: number }>();
  // Asked again within a minute: the code already sent still works.
  if (last && now - last.sent_at < RESEND_MS) return json({ ok: true });
  const code = newCode();
  await env.DB.prepare(
    "INSERT INTO sign_in_codes (email, code_hash, sent_at, expires_at, attempts) VALUES (?1, ?2, ?3, ?4, 0) " +
      "ON CONFLICT(email) DO UPDATE SET code_hash = ?2, sent_at = ?3, expires_at = ?4, attempts = 0",
  )
    .bind(email, await codeHash(email, code), now, now + CODE_TTL_MS)
    .run();
  try {
    await env.EMAIL.send({ from: { email: env.EMAIL_FROM, name: "Jivvy Live" }, to: email, ...signInEmail(code) });
  } catch {
    return json({ ok: false, error: "We couldn't send the code just now. Please try again." }, 502);
  }
  return json({ ok: true });
};
