// POST /api/account/logout: ends this sign-in.
import { SESSION_COOKIE, readCookie, sessionCookie, sha256Hex } from "../../../src/lib/account";
import { json, type Env } from "../../../src/lib/server";

export const onRequestPost: PagesFunction<Env> = async ({ request, env }) => {
  const token = readCookie(request.headers.get("cookie"), SESSION_COOKIE);
  if (token && env.DB) await env.DB.prepare("DELETE FROM sessions WHERE token_hash = ?1").bind(await sha256Hex(token)).run();
  return json({ ok: true }, 200, { "set-cookie": sessionCookie("", 0) });
};
