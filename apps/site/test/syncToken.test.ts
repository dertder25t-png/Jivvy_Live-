import { describe, expect, it } from "vitest";
import { MAX_DEVICES, handleSyncToken, signJwt, signedText, type SyncStore } from "../src/lib/syncToken";

// Test values only: a made-up JWT secret and Supabase project.
const env = { SUPABASE_URL: "https://test-project.supabase.co", SUPABASE_ANON_KEY: "anon-test", SUPABASE_JWT_SECRET: "jwt-secret-test-only" };
const now = Date.UTC(2026, 9, 21);
const b64url = (b: ArrayBuffer) => Buffer.from(b).toString("base64url");

type Row = { churchId: string; name: string; removed: boolean; lastUsed: number };

function store(plusUntil: number) {
  let next = 0;
  const nonces = new Set<string>();
  const devices = new Map<string, Row>();
  const s: SyncStore = {
    useNonce: async (n) => nonces.delete(n),
    device: async (k) => {
      const d = devices.get(k);
      return d ? { churchId: d.churchId, removed: d.removed } : null;
    },
    licenseChurch: async (l) => (l === "JL1.test-license" ? "ch_grace" : null),
    // Like the D1 statement: counted and inserted in one step.
    addDevice: async (k, c, name, t, max) => {
      if (devices.has(k) || [...devices.values()].filter((d) => d.churchId === c && !d.removed).length >= max) return false;
      devices.set(k, { churchId: c, name, removed: false, lastUsed: t });
      return true;
    },
    touchDevice: async (k, t) => void (devices.get(k)!.lastUsed = t),
    plusUntil: async () => plusUntil,
  };
  const nonce = () => {
    const n = `n${next++}`;
    nonces.add(n);
    return n;
  };
  return { s, devices, nonce };
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
  it("gives a registered church computer a 1-hour JWT for its own church, up to the device limit", async () => {
    const { s, devices, nonce } = store(now + 86_400_000);
    const ask = async (d: Awaited<ReturnType<typeof device>>, extra: Record<string, unknown> = {}, n = nonce()) =>
      handleSyncToken({ nonce: n, publicKey: d.publicKey, signature: await d.sign(n), ...extra }, env, s, now);
    const d = await device();
    // First time: the license key registers this computer's device key to the license's church.
    const first = await ask(d, { licenseKey: "JL1.test-license", name: "SANCTUARY-PC" });
    expect(first.status).toBe(200);
    expect(devices.get(d.publicKey)).toMatchObject({ churchId: "ch_grace", name: "SANCTUARY-PC", lastUsed: now });
    const { token, expiresAt, supabaseUrl, supabaseAnonKey } = first.body as Record<string, string>;
    expect([supabaseUrl, supabaseAnonKey, expiresAt]).toEqual([env.SUPABASE_URL, env.SUPABASE_ANON_KEY, now + 3_600_000]);
    expect(claimsOf(token!)).toMatchObject({ role: "authenticated", aud: "authenticated", app_metadata: { church_id: "ch_grace" }, exp: now / 1000 + 3600 });
    // Signed with the JWT secret: the same claims signed again give the same token.
    expect(await signJwt(claimsOf(token!), env.SUPABASE_JWT_SECRET)).toBe(token);
    // After that the device key alone works, and a nonce works once.
    const n = nonce();
    expect((await ask(d, {}, n)).status).toBe(200);
    expect((await ask(d, {}, n)).status).toBe(401);
    // Up to MAX_DEVICES computers; removing one makes room, and the removed one is refused for good.
    for (let i = 1; i < MAX_DEVICES; i++) expect((await ask(await device(), { licenseKey: "JL1.test-license" })).status).toBe(200);
    const extra = await device();
    expect((await ask(extra, { licenseKey: "JL1.test-license" })).status).toBe(403);
    devices.get(d.publicKey)!.removed = true;
    expect((await ask(d, { licenseKey: "JL1.test-license" })).body.error).toBe("device_removed");
    expect((await ask(extra, { licenseKey: "JL1.test-license" })).status).toBe(200);
  });

  it("refuses a church without Plus before registering anything, and an unknown device without a license key", async () => {
    const { s, devices, nonce } = store(now - 1);
    const d = await device();
    const n1 = nonce();
    const noKey = await handleSyncToken({ nonce: n1, publicKey: d.publicKey, signature: await d.sign(n1) }, env, s, now);
    expect(noKey.body.error).toBe("unknown_device");
    const n2 = nonce();
    const r = await handleSyncToken({ nonce: n2, publicKey: d.publicKey, signature: await d.sign(n2), licenseKey: "JL1.test-license" }, env, s, now);
    expect(r).toEqual({ status: 403, body: { ok: false, error: "no_plus" } });
    expect(devices.size).toBe(0);
  });
});
