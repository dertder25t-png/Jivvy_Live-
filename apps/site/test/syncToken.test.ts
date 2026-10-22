import { describe, expect, it } from "vitest";
import { handleSyncToken, signJwt, signedText, type SyncStore } from "../src/lib/syncToken";

// Test values only: a made-up JWT secret and Supabase project.
const env = { SUPABASE_URL: "https://test-project.supabase.co", SUPABASE_ANON_KEY: "anon-test", SUPABASE_JWT_SECRET: "jwt-secret-test-only" };
const now = Date.UTC(2026, 9, 21);
const b64url = (b: ArrayBuffer) => Buffer.from(b).toString("base64url");

function store(plusUntil: number) {
  const nonces = new Set(["n1", "n2", "n3"]);
  const devices = new Map<string, string>();
  const s: SyncStore = {
    useNonce: async (n) => nonces.delete(n),
    deviceChurch: async (k) => devices.get(k) ?? null,
    licenseChurch: async (l) => (l === "JL1.test-license" ? "ch_grace" : null),
    deviceCount: async (c) => [...devices.values()].filter((x) => x === c).length,
    addDevice: async (k, c) => void devices.set(k, c),
    plusUntil: async () => plusUntil,
  };
  return { s, devices };
}

async function device() {
  const pair = (await crypto.subtle.generateKey({ name: "ECDSA", namedCurve: "P-256" }, true, ["sign", "verify"])) as CryptoKeyPair;
  const publicKey = b64url(await crypto.subtle.exportKey("raw", pair.publicKey));
  const sign = async (nonce: string) =>
    b64url(await crypto.subtle.sign({ name: "ECDSA", hash: "SHA-256" }, pair.privateKey, new TextEncoder().encode(signedText(nonce, publicKey))));
  return { publicKey, sign };
}

const claimsOf = (jwt: string) => JSON.parse(Buffer.from(jwt.split(".")[1]!, "base64url").toString());

describe("sync token exchange", () => {
  it("gives a registered church computer a 1-hour JWT for its own church", async () => {
    const { s, devices } = store(now + 86_400_000);
    const d = await device();
    // First time: the license key registers this computer's device key to the license's church.
    const first = await handleSyncToken({ nonce: "n1", publicKey: d.publicKey, signature: await d.sign("n1"), licenseKey: "JL1.test-license" }, env, s, now);
    expect(first.status).toBe(200);
    expect(devices.get(d.publicKey)).toBe("ch_grace");
    const { token, expiresAt, supabaseUrl, supabaseAnonKey } = first.body as Record<string, string>;
    expect([supabaseUrl, supabaseAnonKey, expiresAt]).toEqual([env.SUPABASE_URL, env.SUPABASE_ANON_KEY, now + 3_600_000]);
    expect(claimsOf(token!)).toMatchObject({ role: "authenticated", aud: "authenticated", app_metadata: { church_id: "ch_grace" }, exp: now / 1000 + 3600 });
    // Signed with the JWT secret: the same claims signed again give the same token.
    expect(await signJwt(claimsOf(token!), env.SUPABASE_JWT_SECRET)).toBe(token);
    // After that the device key alone works, and a nonce works once.
    const again = await handleSyncToken({ nonce: "n2", publicKey: d.publicKey, signature: await d.sign("n2") }, env, s, now);
    expect(again.status).toBe(200);
    const replay = await handleSyncToken({ nonce: "n2", publicKey: d.publicKey, signature: await d.sign("n2") }, env, s, now);
    expect(replay.status).toBe(401);
  });

  it("refuses a church without Plus, and an unknown device without a license key", async () => {
    const { s } = store(now - 1);
    const d = await device();
    const noKey = await handleSyncToken({ nonce: "n1", publicKey: d.publicKey, signature: await d.sign("n1") }, env, s, now);
    expect(noKey.body.error).toBe("unknown_device");
    const r = await handleSyncToken({ nonce: "n2", publicKey: d.publicKey, signature: await d.sign("n2"), licenseKey: "JL1.test-license" }, env, s, now);
    expect(r).toEqual({ status: 403, body: { ok: false, error: "no_plus" } });
  });
});
