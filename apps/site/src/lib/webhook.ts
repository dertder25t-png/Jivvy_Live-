// The Stripe webhook: a paid checkout issues the church's license key.
import { signLicenseKey } from "@jivvy/protocol";
import { churchLicense, isTestSecretKey, paidCheckout, verifyStripeSignature } from "./stripe";

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
  /** Whether a church account with this id exists. */
  hasChurch(churchId: string): Promise<boolean>;
}

export interface WebhookEnv {
  STRIPE_WEBHOOK_SECRET?: string;
  /** Used for test-mode purchases (the published fixture key). Production's LICENSE_PRIVATE_KEY is never used here. */
  LICENSE_TEST_PRIVATE_KEY?: string;
  /** Test mode only for now: the key type must match the events' livemode. */
  STRIPE_SECRET_KEY?: string;
}

/** Answers one webhook delivery. Only a valid `Stripe-Signature` gets past the first line. */
export async function handleStripeWebhook(
  body: string,
  signature: string | null,
  env: WebhookEnv,
  store: LicenseStore,
  nowMs: number,
): Promise<{ status: number; body: Record<string, unknown> }> {
  // The key type decides everything: a test key means test events, signed with the test license key.
  if (!env.STRIPE_WEBHOOK_SECRET || !env.LICENSE_TEST_PRIVATE_KEY || !isTestSecretKey(env.STRIPE_SECRET_KEY)) return { status: 503, body: { error: "not configured" } };
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
  // Paid for a church that doesn't exist (deleted, or a forged reference): retrying can't fix it, so
  // answer 200 (Stripe stops retrying) and log the session id, never a key.
  if (!(await store.hasChurch(paid.churchId))) {
    console.error(`stripe webhook: no church for checkout ${paid.sessionId}; nothing issued`);
    return { status: 200, body: { received: true } };
  }
  const payload = churchLicense(paid.churchId, nowMs);
  const key = await signLicenseKey(payload, env.LICENSE_TEST_PRIVATE_KEY);
  // A failure here answers 500, so Stripe delivers the event again.
  await store.save({ churchId: paid.churchId, stripeSession: paid.sessionId, key, ...payload });
  return { status: 200, body: { received: true, issued: true } };
}
