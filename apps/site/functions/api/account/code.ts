// POST /api/account/code { email }: emails a one-time sign-in code to that address only.
import { CODE_TTL_MS, RESEND_MS, codeHash, newCode, normalEmail, signInEmail } from "../../../src/lib/account";
import { clientKey, guardPost, json, readJson, withinLimit, type Env } from "../../../src/lib/server";

/** Codes sent per caller per hour (each send is an email). */
const SENDS_PER_HOUR = 10;

export const onRequestPost: PagesFunction<Env> = async ({ request, env }) => {
  const blocked = guardPost(request);
  if (blocked) return blocked;
  const email = normalEmail((await readJson(request)).email);
  if (!email) return json({ ok: false, error: "Please enter a valid email address." }, 400);
  if (!env.DB || !env.EMAIL || !env.EMAIL_FROM) return json({ ok: false, error: "Sign-in isn't open yet." }, 503);
  const now = Date.now();
  if (!(await withinLimit(env.DB, `code:${await clientKey(request)}`, SENDS_PER_HOUR, 60 * 60_000, now)))
    return json({ ok: false, error: "Too many tries. Please wait a while and try again." }, 429);
  const last = await env.DB.prepare("SELECT sent_at FROM sign_in_codes WHERE email = ?1").bind(email).first<{ sent_at: number }>();
  // Asked again within a minute: the code already sent still works.
  if (last && now - last.sent_at < RESEND_MS) return json({ ok: true });
  const code = newCode();
  const hash = await codeHash(email, code);
  // A resend inside the old code's lifetime keeps its wrong-try count, so resending never buys more guesses.
  await env.DB.prepare(
    "INSERT INTO sign_in_codes (email, code_hash, sent_at, expires_at, attempts) VALUES (?1, ?2, ?3, ?4, 0) " +
      "ON CONFLICT(email) DO UPDATE SET code_hash = ?2, sent_at = ?3, " +
      "attempts = CASE WHEN expires_at > ?3 THEN attempts ELSE 0 END, expires_at = ?4",
  )
    .bind(email, hash, now, now + CODE_TTL_MS)
    .run();
  try {
    await env.EMAIL.send({ from: { email: env.EMAIL_FROM, name: "Jivvy Live" }, to: email, ...signInEmail(code) });
  } catch {
    // Not delivered: lift the one-a-minute throttle so the retry really sends a fresh code.
    await env.DB.prepare("UPDATE sign_in_codes SET sent_at = 0 WHERE email = ?1 AND code_hash = ?2").bind(email, hash).run();
    return json({ ok: false, error: "We couldn't send the code just now. Please try again." }, 502);
  }
  return json({ ok: true });
};
