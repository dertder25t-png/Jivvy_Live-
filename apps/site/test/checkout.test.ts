import { describe, expect, it } from "vitest";
import { verifyLicenseKey } from "@jivvy/protocol";
import fixture from "../../../packages/protocol/fixtures/license.json";
import { stripeSignature } from "../src/lib/stripe";
import { handleStripeWebhook, type IssuedLicense } from "../src/lib/webhook";
import { codeHash, readCookie, sessionCookie } from "../src/lib/account";

// Test values only: a made-up webhook secret and the fixture's published test signing key.
const env = { STRIPE_WEBHOOK_SECRET: "whsec_test_only", LICENSE_PRIVATE_KEY: fixture.testOnlyPrivateKeyPkcs8 };
const now = Date.UTC(2026, 9, 21);
const paid = JSON.stringify({
  type: "checkout.session.completed",
  livemode: false,
  data: { object: { id: "cs_test_1", payment_status: "paid", client_reference_id: "ch_test" } },
});

function store() {
  const saved: IssuedLicense[] = [];
  return { saved, save: async (l: IssuedLicense) => void saved.push(l) };
}
const signed = async (body: string, t = now / 1000) => `t=${t},v1=${await stripeSignature(body, t, env.STRIPE_WEBHOOK_SECRET)}`;

describe("Stripe webhook", () => {
  it("issues a license key the church computer accepts for a paid test checkout", async () => {
    const s = store();
    const r = await handleStripeWebhook(paid, await signed(paid), env, s, now);
    expect(r).toEqual({ status: 200, body: { received: true, issued: true } });
    expect(s.saved).toHaveLength(1);
    const payload = await verifyLicenseKey(s.saved[0]!.key, fixture.publicKey);
    expect(payload).toEqual({ church: "ch_test", plan: "church", updatesUntil: Date.UTC(2029, 9, 21), plusUntil: Date.UTC(2027, 9, 21), issued: now });
  });

  it("refuses a bad or stale signature and ignores live-mode events", async () => {
    const s = store();
    expect((await handleStripeWebhook(paid, `t=${now / 1000},v1=${"0".repeat(64)}`, env, s, now)).status).toBe(400);
    expect((await handleStripeWebhook(paid, await signed(paid, now / 1000 - 3600), env, s, now)).status).toBe(400);
    const live = paid.replace('"livemode":false', '"livemode":true');
    expect(await handleStripeWebhook(live, await signed(live), env, s, now)).toEqual({ status: 200, body: { received: true } });
    expect(s.saved).toHaveLength(0);
  });
});

describe("sign-in", () => {
  it("binds a code to its address and reads the session cookie back", async () => {
    expect(await codeHash("one@example.org", "123456")).not.toBe(await codeHash("two@example.org", "123456"));
    expect(readCookie(`x=1; ${sessionCookie("tok", 1000).split(";")[0]}`, "jl_session")).toBe("tok");
  });
});
