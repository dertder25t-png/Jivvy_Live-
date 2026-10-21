// Stripe Checkout and its webhook. Test mode only for now: live keys and live events are refused.
import type { LicensePayload } from "@jivvy/protocol";

/** How old a webhook's signed timestamp may be (Stripe's default tolerance). */
export const SIGNATURE_TOLERANCE_S = 300;

export const isTestSecretKey = (k: string | undefined): k is string => typeof k === "string" && k.startsWith("sk_test_");

const hex = (b: ArrayBuffer) => [...new Uint8Array(b)].map((x) => x.toString(16).padStart(2, "0")).join("");

/** Equal strings, compared in constant time for equal lengths. */
function same(a: string, b: string): boolean {
  if (a.length !== b.length) return false;
  let diff = 0;
  for (let i = 0; i < a.length; i++) diff |= a.charCodeAt(i) ^ b.charCodeAt(i);
  return diff === 0;
}

/** HMAC-SHA256 of `${t}.${body}` with the endpoint's secret, hex: what Stripe puts in `v1`. */
export async function stripeSignature(body: string, t: number, secret: string): Promise<string> {
  const key = await crypto.subtle.importKey("raw", new TextEncoder().encode(secret), { name: "HMAC", hash: "SHA-256" }, false, ["sign"]);
  return hex(await crypto.subtle.sign("HMAC", key, new TextEncoder().encode(`${t}.${body}`)));
}

/** Checks a `Stripe-Signature` header (`t=...,v1=...[,v1=...]`) against the raw body. */
export async function verifyStripeSignature(body: string, header: string | null, secret: string, nowMs: number): Promise<boolean> {
  if (!header || !secret) return false;
  const parts = header.split(",").map((p) => p.trim().split("=") as [string, string]);
  const t = Number(parts.find(([k]) => k === "t")?.[1]);
  const v1 = parts.filter(([k]) => k === "v1").map(([, v]) => v ?? "");
  if (!Number.isSafeInteger(t) || v1.length === 0) return false;
  if (Math.abs(nowMs / 1000 - t) > SIGNATURE_TOLERANCE_S) return false;
  const expected = await stripeSignature(body, t, secret);
  return v1.some((s) => same(s, expected));
}

/** The form body for POST /v1/checkout/sessions: one church license, paid once. */
export function checkoutForm(o: { priceId: string; churchId: string; email: string; siteUrl: string }): URLSearchParams {
  return new URLSearchParams({
    mode: "payment",
    "line_items[0][price]": o.priceId,
    "line_items[0][quantity]": "1",
    client_reference_id: o.churchId,
    customer_email: o.email,
    "metadata[church]": o.churchId,
    success_url: `${o.siteUrl}/account?paid=1`,
    cancel_url: `${o.siteUrl}/account`,
  });
}

export interface PaidCheckout {
  sessionId: string;
  churchId: string;
}

/** A completed, paid, test-mode checkout from a webhook event; null for anything else. */
export function paidCheckout(event: unknown): PaidCheckout | null {
  if (typeof event !== "object" || event === null) return null;
  const e = event as { type?: unknown; livemode?: unknown; data?: { object?: Record<string, unknown> } };
  const s = e.data?.object;
  if (e.type !== "checkout.session.completed" || e.livemode !== false || !s) return null;
  if (s.payment_status !== "paid" || typeof s.id !== "string" || typeof s.client_reference_id !== "string") return null;
  return { sessionId: s.id, churchId: s.client_reference_id };
}

/** What a $200 church license unlocks: 3 years of new features and the first year of Plus. */
export function churchLicense(churchId: string, nowMs: number): LicensePayload {
  const years = (n: number) => {
    const d = new Date(nowMs);
    d.setUTCFullYear(d.getUTCFullYear() + n);
    return d.getTime();
  };
  return { church: churchId, plan: "church", updatesUntil: years(3), plusUntil: years(1), issued: nowMs };
}
