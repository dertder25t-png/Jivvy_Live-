// POST /api/account/verify { email, code }: signs in (making the church account on first sign-in).
import { CODE_DIGITS, MAX_CODE_ATTEMPTS, SESSION_TTL_MS, codeHash, newChurchId, newToken, normalEmail, sessionCookie, sha256Hex, typedCode } from "../../../src/lib/account";
import { clientKey, guardPost, json, readJson, withinLimit, type Env } from "../../../src/lib/server";

/** Verify tries per caller per 10 minutes, across all addresses. */
const TRIES_PER_WINDOW = 30;

export const onRequestPost: PagesFunction<Env> = async ({ request, env }) => {
  const blocked = guardPost(request);
  if (blocked) return blocked;
  const data = await readJson(request);
  const email = normalEmail(data.email);
  const code = typedCode(data.code);
  if (!email || code.length !== CODE_DIGITS) return json({ ok: false, error: `Enter your email and the ${CODE_DIGITS}-digit code.` }, 400);
  if (!env.DB) return json({ ok: false, error: "Sign-in isn't open yet." }, 503);
  const db = env.DB;
  const now = Date.now();
  if (!(await withinLimit(db, `verify:${await clientKey(request)}`, TRIES_PER_WINDOW, 10 * 60_000, now)))
    return json({ ok: false, error: "Too many tries. Please wait a while and try again." }, 429);
  const wrong = () => json({ ok: false, error: "That code didn't work. Check it, or ask for a new one." }, 400);
  // Count the try first, atomically, so parallel guesses can't beat the limit.
  const row = await db
    .prepare("UPDATE sign_in_codes SET attempts = attempts + 1 WHERE email = ?1 AND attempts < ?2 AND expires_at > ?3 RETURNING code_hash")
    .bind(email, MAX_CODE_ATTEMPTS, now)
    .first<{ code_hash: string }>();
  if (!row) return wrong();
  const hash = await codeHash(email, code);
  if (row.code_hash !== hash) return wrong();
  // Single use: only the request whose DELETE removes the row signs in.
  const used = await db.prepare("DELETE FROM sign_in_codes WHERE email = ?1 AND code_hash = ?2 RETURNING email").bind(email, hash).first();
  if (!used) return wrong();
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
