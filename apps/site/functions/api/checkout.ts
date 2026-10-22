// POST /api/checkout: a Stripe Checkout Session for the signed-in church (test mode only).
import { checkoutForm, isTestSecretKey } from "../../src/lib/stripe";
import { guardPost, json, signedIn, type Env } from "../../src/lib/server";

export const onRequestPost: PagesFunction<Env> = async ({ request, env }) => {
  const blocked = guardPost(request);
  if (blocked) return blocked;
  if (!env.DB) return json({ ok: false, error: "Accounts aren't open yet." }, 503);
  // Never a live key: no real charge until the owner decides to switch.
  if (!isTestSecretKey(env.STRIPE_SECRET_KEY) || !env.STRIPE_PRICE_ID || !env.SITE_URL)
    return json({ ok: false, error: "Checkout isn't open yet." }, 503);
  const church = await signedIn(request, env.DB, Date.now());
  if (!church) return json({ ok: false, error: "Please sign in first." }, 401);
  const res = await fetch("https://api.stripe.com/v1/checkout/sessions", {
    method: "POST",
    headers: { authorization: `Bearer ${env.STRIPE_SECRET_KEY}`, "content-type": "application/x-www-form-urlencoded" },
    body: checkoutForm({ priceId: env.STRIPE_PRICE_ID, churchId: church.id, email: church.email, siteUrl: env.SITE_URL }),
  });
  const session = (await res.json().catch(() => null)) as { url?: string } | null;
  if (!res.ok || !session?.url) return json({ ok: false, error: "Checkout couldn't start. Please try again." }, 502);
  return json({ ok: true, url: session.url });
};
