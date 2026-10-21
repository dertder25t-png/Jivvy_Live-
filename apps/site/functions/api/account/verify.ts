// POST /api/account/verify { email, code }: signs in (making the church account on first sign-in).
import { MAX_CODE_ATTEMPTS, SESSION_TTL_MS, codeHash, newChurchId, newToken, normalEmail, sessionCookie, sha256Hex, typedCode } from "../../../src/lib/account";
import { json, readBody, type Env } from "../../../src/lib/server";

export const onRequestPost: PagesFunction<Env> = async ({ request, env }) => {
  const data = await readBody(request);
  const email = normalEmail(data.email);
  const code = typedCode(data.code);
  if (!email || code.length !== 6) return json({ ok: false, error: "Enter your email and the 6-digit code." }, 400);
  if (!env.DB) return json({ ok: false, error: "Sign-in isn't open yet." }, 503);
  const db = env.DB;
  const now = Date.now();
  const row = await db
    .prepare("SELECT code_hash, expires_at, attempts FROM sign_in_codes WHERE email = ?1")
    .bind(email)
    .first<{ code_hash: string; expires_at: number; attempts: number }>();
  const wrong = { ok: false, error: "That code didn't work. Check it, or ask for a new one." };
  if (!row || row.expires_at <= now || row.attempts >= MAX_CODE_ATTEMPTS) return json(wrong, 400);
  if (row.code_hash !== (await codeHash(email, code))) {
    await db.prepare("UPDATE sign_in_codes SET attempts = attempts + 1 WHERE email = ?1").bind(email).run();
    return json(wrong, 400);
  }
  await db.prepare("DELETE FROM sign_in_codes WHERE email = ?1").bind(email).run();
  await db.prepare("INSERT OR IGNORE INTO churches (id, email, created_at) VALUES (?1, ?2, ?3)").bind(newChurchId(), email, now).run();
  const church = await db.prepare("SELECT id FROM churches WHERE email = ?1").bind(email).first<{ id: string }>();
  if (!church) return json({ ok: false, error: "Something went wrong on our side. Please try again." }, 500);
  const token = newToken();
  await db
    .prepare("INSERT INTO sessions (token_hash, church_id, expires_at) VALUES (?1, ?2, ?3)")
    .bind(await sha256Hex(token), church.id, now + SESSION_TTL_MS)
    .run();
  return json({ ok: true }, 200, { "set-cookie": sessionCookie(token, SESSION_TTL_MS) });
};
