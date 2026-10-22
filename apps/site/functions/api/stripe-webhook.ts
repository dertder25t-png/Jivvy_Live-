// POST /api/stripe-webhook: Stripe tells us a checkout was paid; we issue the license key.
import { handleStripeWebhook, type LicenseStore } from "../../src/lib/webhook";
import { json, type Env } from "../../src/lib/server";

export const onRequestPost: PagesFunction<Env> = async ({ request, env }) => {
  if (!env.DB) return json({ error: "not configured" }, 503);
  const db = env.DB;
  const store: LicenseStore = {
    async hasChurch(id) {
      return !!(await db.prepare("SELECT 1 AS x FROM churches WHERE id = ?1").bind(id).first());
    },
    async save(l) {
      await db
        .prepare(
          "INSERT OR IGNORE INTO licenses (church_id, stripe_session, license_key, plan, updates_until, plus_until, issued) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )
        .bind(l.churchId, l.stripeSession, l.key, l.plan, l.updatesUntil, l.plusUntil, l.issued)
        .run();
    },
  };
  // The signature is over the exact bytes Stripe sent, so read the body as text before parsing.
  const body = await request.text();
  try {
    const r = await handleStripeWebhook(body, request.headers.get("stripe-signature"), env, store, Date.now());
    return json(r.body, r.status);
  } catch {
    return json({ error: "try again" }, 500);
  }
};
