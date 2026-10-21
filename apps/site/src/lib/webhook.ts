// The Stripe webhook: a paid checkout issues the church's license key.
import { signLicenseKey } from "@jivvy/protocol";
import { churchLicense, paidCheckout, verifyStripeSignature } from "./stripe";

export interface IssuedLicense {
  churchId: string;
  stripeSession: string;
  key: string;
  plan: string;
  updatesUntil: number;
  plusUntil: number;
  issued: number;
}

/** Where licenses go. `save` is a no-op for a checkout already issued (Stripe retries). */
export interface LicenseStore {
  save(license: IssuedLicense): Promise<void>;
}

export interface WebhookEnv {
  STRIPE_WEBHOOK_SECRET?: string;
  LICENSE_PRIVATE_KEY?: string;
}

/** Answers one webhook delivery. Only a valid `Stripe-Signature` gets past the first line. */
export async function handleStripeWebhook(
  body: string,
  signature: string | null,
  env: WebhookEnv,
  store: LicenseStore,
  nowMs: number,
): Promise<{ status: number; body: Record<string, unknown> }> {
  if (!env.STRIPE_WEBHOOK_SECRET || !env.LICENSE_PRIVATE_KEY) return { status: 503, body: { error: "not configured" } };
  if (!(await verifyStripeSignature(body, signature, env.STRIPE_WEBHOOK_SECRET, nowMs)))
    return { status: 400, body: { error: "bad signature" } };
  let event: unknown;
  try {
    event = JSON.parse(body);
  } catch {
    return { status: 400, body: { error: "bad body" } };
  }
  const paid = paidCheckout(event);
  // Other events, unpaid sessions and anything in live mode: acknowledged, nothing issued.
  if (!paid) return { status: 200, body: { received: true } };
  const payload = churchLicense(paid.churchId, nowMs);
  const key = await signLicenseKey(payload, env.LICENSE_PRIVATE_KEY);
  // A failure here answers 500, so Stripe delivers the event again.
  await store.save({ churchId: paid.churchId, stripeSession: paid.sessionId, key, ...payload });
  return { status: 200, body: { received: true, issued: true } };
}
