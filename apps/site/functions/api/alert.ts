// POST /api/alert { key, kind, message }: emails a stream-down or no-sound alert to the church
// account's own address. Authenticated by the license key's signature and church id, not a cookie.
import { KIND_WINDOW_MS, PER_DAY, handleAlert, type AlertStore } from "../../src/lib/alertEmail";
import { json, readJson, withinLimit, type Env } from "../../src/lib/server";

export const onRequestPost: PagesFunction<Env> = async ({ request, env }) => {
  if (!env.DB || !env.EMAIL || !env.EMAIL_FROM) return json({ ok: false, error: "Alert emails aren't open yet." }, 503);
  const { DB: db, EMAIL: email, EMAIL_FROM: from } = env;
  const store: AlertStore = {
    async churchEmail(id) {
      return (await db.prepare("SELECT email FROM churches WHERE id = ?1").bind(id).first<{ email: string }>())?.email ?? null;
    },
    async lastSent(id, kind) {
      return (await db.prepare("SELECT sent_at FROM alert_emails WHERE church_id = ?1 AND kind = ?2").bind(id, kind).first<{ sent_at: number }>())?.sent_at ?? 0;
    },
    async claim(id, kind, now) {
      const before = (await db.prepare("SELECT sent_at FROM alert_emails WHERE church_id = ?1 AND kind = ?2").bind(id, kind).first<{ sent_at: number }>())?.sent_at ?? 0;
      // One atomic upsert: only a request that finds the slot free takes it.
      const row = await db
        .prepare(
          "INSERT INTO alert_emails (church_id, kind, sent_at) VALUES (?1, ?2, ?3) " +
            "ON CONFLICT(church_id, kind) DO UPDATE SET sent_at = ?3 WHERE sent_at <= ?3 - ?4 RETURNING sent_at",
        )
        .bind(id, kind, now, KIND_WINDOW_MS)
        .first();
      return row ? before : null;
    },
    async release(id, kind, previous, now) {
      await db.prepare("UPDATE alert_emails SET sent_at = ?3 WHERE church_id = ?1 AND kind = ?2 AND sent_at = ?4").bind(id, kind, previous, now).run();
    },
    underDailyLimit: (id, now) => withinLimit(db, `alert-day:${id}`, PER_DAY, 24 * 60 * 60_000, now),
    async send(to, subject, text) {
      await email.send({ from: { email: from, name: "Jivvy Live" }, to, subject, text });
    },
  };
  const answer = await handleAlert(await readJson(request), env, store, Date.now());
  return json(answer.body, answer.status);
};
