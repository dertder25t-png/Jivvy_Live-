import { describe, expect, it } from "vitest";
import { verifyLicenseKey } from "@jivvy/protocol";
import fixture from "../../../packages/protocol/fixtures/license.json";
import { isTestSecretKey, stripeSignature } from "../src/lib/stripe";
import { handleStripeWebhook, type IssuedLicense } from "../src/lib/webhook";
import { CODE_DIGITS, SESSION_COOKIE, codeHash, newCode, normalEmail, readCookie, sessionCookie } from "../src/lib/account";
import { guardPost } from "../src/lib/server";

// Test values only: a made-up webhook secret and the fixture's published test signing key.
const env = {
  STRIPE_WEBHOOK_SECRET: "whsec_test_only",
  STRIPE_SECRET_KEY: "sk_test_only",
  LICENSE_TEST_PRIVATE_KEY: fixture.testOnlyPrivateKeyPkcs8,
};
const now = Date.UTC(2026, 9, 21);
const event = (over: Record<string, unknown> = {}, type = "checkout.session.completed", livemode = false) =>
  JSON.stringify({
    type,
    livemode,
    data: {
      object: { id: "cs_test_1", mode: "payment", amount_total: 20000, currency: "usd", payment_status: "paid", client_reference_id: "ch_test", ...over },
    },
  });
const paid = event();

function store(hasChurch = true) {
  const saved: IssuedLicense[] = [];
  return { saved, save: async (l: IssuedLicense) => void saved.push(l), hasChurch: async () => hasChurch };
}
const signed = async (body: string, t = now / 1000) => `t=${t},v1=${await stripeSignature(body, t, env.STRIPE_WEBHOOK_SECRET)}`;
const deliver = async (body: string, s = store(), e: Record<string, string> = env) => handleStripeWebhook(body, await signed(body), e, s, now);

describe("Stripe webhook", () => {
  it("issues a license key the church computer accepts for a paid test checkout", async () => {
    const s = store();
    const r = await deliver(paid, s);
    expect(r).toEqual({ status: 200, body: { received: true, issued: true } });
    expect(s.saved).toHaveLength(1);
    const payload = await verifyLicenseKey(s.saved[0]!.key, fixture.publicKey);
    expect(payload).toEqual({ church: "ch_test", plan: "church", updatesUntil: Date.UTC(2029, 9, 21), plusUntil: Date.UTC(2027, 9, 21), issued: now });
  });

  it("refuses a bad or stale signature and ignores live-mode events", async () => {
    const s = store();
    expect((await handleStripeWebhook(paid, `t=${now / 1000},v1=${"0".repeat(64)}`, env, s, now)).status).toBe(400);
    expect((await handleStripeWebhook(paid, await signed(paid, now / 1000 - 3600), env, s, now)).status).toBe(400);
    expect(await deliver(event({}, "checkout.session.completed", true), s)).toEqual({ status: 200, body: { received: true } });
    expect(s.saved).toHaveLength(0);
  });

  it("never signs with the production key and needs a test Stripe key", async () => {
    const s = store();
    const prod = { ...env, LICENSE_PRIVATE_KEY: "production-key-must-not-be-used" } as Record<string, string>;
    delete prod.LICENSE_TEST_PRIVATE_KEY;
    expect((await deliver(paid, s, prod)).status).toBe(503);
    expect((await deliver(paid, s, { ...env, STRIPE_SECRET_KEY: "sk_live_x" })).status).toBe(503);
    expect(s.saved).toHaveLength(0);
    expect(isTestSecretKey("rk_test_abc")).toBe(true);
    expect(isTestSecretKey("rk_live_abc")).toBe(false);
  });

  it("issues only for the church license: mode, amount and currency must match", async () => {
    const s = store();
    for (const over of [{ mode: "subscription" }, { amount_total: 100 }, { currency: "eur" }, { payment_status: "unpaid" }]) {
      expect(await deliver(event(over), s)).toEqual({ status: 200, body: { received: true } });
    }
    expect(s.saved).toHaveLength(0);
  });

  it("issues when a delayed payment settles", async () => {
    const s = store();
    const r = await deliver(event({}, "checkout.session.async_payment_succeeded"), s);
    expect(r.body).toEqual({ received: true, issued: true });
    expect(s.saved).toHaveLength(1);
  });

  it("answers 200 without a key when the church is gone, so Stripe stops retrying", async () => {
    const s = store(false);
    expect(await deliver(paid, s)).toEqual({ status: 200, body: { received: true } });
    expect(s.saved).toHaveLength(0);
  });
});

describe("sign-in", () => {
  it("binds a code to its address and reads the session cookie back", async () => {
    expect(await codeHash("one@example.org", "12345678")).not.toBe(await codeHash("two@example.org", "12345678"));
    const cookie = sessionCookie("tok", 1000);
    expect(cookie.startsWith("__Host-")).toBe(true);
    expect(cookie).toContain("Secure");
    expect(cookie).toContain("Path=/");
    expect(cookie).not.toContain("Domain");
    expect(readCookie(`x=1; ${cookie.split(";")[0]}`, SESSION_COOKIE)).toBe("tok");
  });

  it("makes 8-digit codes", () => {
    for (let i = 0; i < 50; i++) expect(newCode()).toMatch(new RegExp(`^\\d{${CODE_DIGITS}}$`));
    expect(CODE_DIGITS).toBe(8);
  });

  it("accepts plain addresses and rejects odd ones", () => {
    expect(normalEmail("  Pastor.Sam+church@Example.org ")).toBe("pastor.sam+church@example.org");
    for (const bad of ["a@b", "a@b.c", "a b@example.org", "a@@example.org", "a@example..org", ".a@example.org", "a@-example.org", "a@example.org,b@example.org", "a@example.123"])
      expect(normalEmail(bad)).toBeNull();
  });
});

describe("POST guard", () => {
  const post = (headers: Record<string, string>) => new Request("https://live.example/api/account/code", { method: "POST", headers, body: "{}" });
  const json = { "content-type": "application/json" };
  it("takes JSON from the site's own origin only", () => {
    expect(guardPost(post({ ...json, origin: "https://live.example" }))).toBeNull();
    expect(guardPost(post({ ...json, "sec-fetch-site": "same-origin" }))).toBeNull();
    expect(guardPost(post({ ...json, origin: "https://evil.example" }))?.status).toBe(403);
    expect(guardPost(post({ ...json, "sec-fetch-site": "cross-site" }))?.status).toBe(403);
    expect(guardPost(post(json))?.status).toBe(403);
    expect(guardPost(post({ "content-type": "application/x-www-form-urlencoded", origin: "https://live.example" }))?.status).toBe(415);
    expect(guardPost(post({ "content-type": "text/plain", origin: "https://live.example" }))?.status).toBe(415);
  });
});
