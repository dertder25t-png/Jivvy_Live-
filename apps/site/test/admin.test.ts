import { describe, expect, it } from "vitest";
import { adminEmails, requireAdmin, teamOrigin, waitlistCsv, type FetchKeys } from "../src/lib/admin";

// Test values only: a made-up Access team and audience, and a key pair made for this run.
const team = "https://jivvy-test.cloudflareaccess.com";
const aud = "aud-test-only";
const now = Date.UTC(2026, 10, 10);
const b64url = (b: ArrayBuffer | Uint8Array) => Buffer.from(b instanceof Uint8Array ? b : new Uint8Array(b)).toString("base64url");

async function issuer() {
  const pair = (await crypto.subtle.generateKey(
    { name: "RSASSA-PKCS1-v1_5", modulusLength: 2048, publicExponent: new Uint8Array([1, 0, 1]), hash: "SHA-256" },
    true,
    ["sign", "verify"],
  )) as CryptoKeyPair;
  const jwk = { ...(await crypto.subtle.exportKey("jwk", pair.publicKey)), kid: "kid-1" };
  const fetchKeys: FetchKeys = async (url) => (url === `${team}/cdn-cgi/access/certs` ? [jwk] : []);
  const sign = async (claims: Record<string, unknown>, kid = "kid-1") => {
    const h = b64url(new TextEncoder().encode(JSON.stringify({ alg: "RS256", kid })));
    const p = b64url(new TextEncoder().encode(JSON.stringify(claims)));
    const s = await crypto.subtle.sign("RSASSA-PKCS1-v1_5", pair.privateKey, new TextEncoder().encode(`${h}.${p}`));
    return `${h}.${p}.${b64url(s)}`;
  };
  return { fetchKeys, sign };
}

const env = { DB: {} as D1Database, ADMIN_EMAILS: "owner@example.org", ACCESS_TEAM_DOMAIN: team, ACCESS_AUD: aud };
const asking = (token?: string) =>
  new Request("https://live.example/api/admin/waitlist", { headers: token ? { "cf-access-jwt-assertion": token } : {} });
const good = { aud: [aud], iss: team, email: "Owner@Example.org", exp: now / 1000 + 600, nbf: now / 1000 - 5 };

describe("admin page", () => {
  it("reads its settings", () => {
    expect([...adminEmails(" Owner@Example.org, second@example.org\nnot-an-email ")]).toEqual(["owner@example.org", "second@example.org"]);
    expect(teamOrigin("jivvy")).toBe("https://jivvy.cloudflareaccess.com");
    expect(teamOrigin("https://Jivvy.cloudflareaccess.com/")).toBe("https://jivvy.cloudflareaccess.com");
    expect(teamOrigin("https://evil.example")).toBeNull();
  });

  it("lets in only a valid Cloudflare Access token for this app whose email is an admin", async () => {
    const { fetchKeys, sign } = await issuer();
    const status = async (token: string | undefined, e: Record<string, unknown> = env) => {
      const r = await requireAdmin(asking(token), e, now, fetchKeys);
      return typeof r === "string" ? r : r.status;
    };
    expect(await status(await sign(good))).toBe("owner@example.org");
    expect(await status(await sign({ ...good, email: "grace@example.org" }))).toBe(403);
    expect(await status(undefined)).toBe(401);
    expect(await status(await sign({ ...good, aud: ["another-app"] }))).toBe(401);
    expect(await status(await sign({ ...good, iss: "https://evil.cloudflareaccess.com" }))).toBe(401);
    expect(await status(await sign({ ...good, exp: now / 1000 - 1 }))).toBe(401);
    expect(await status(await sign(good, "unknown-kid"))).toBe(401);
    // Claims changed after signing.
    const [h, , s] = (await sign(good)).split(".");
    const forged = b64url(new TextEncoder().encode(JSON.stringify({ ...good, email: "owner@example.org", exp: now / 1000 + 99999 })));
    expect(await status(`${h}.${forged}.${s}`)).toBe(401);
    // A token signed by some other key under the same kid.
    expect(await status(await (await issuer()).sign(good))).toBe(401);
    // Not set up: closed to everyone.
    expect(await status(await sign(good), { ...env, ADMIN_EMAILS: "" })).toBe(503);
    expect(await status(await sign(good), { ...env, ACCESS_AUD: undefined })).toBe(503);
  });

  it("exports the waitlist as CSV a spreadsheet won't run as formulas", () => {
    const csv = waitlistCsv([
      { email: "a@example.org", church: 'Grace "Main"', size: "50-150", software: "=HYPERLINK(1)", alphaTester: true, createdAt: "2026-11-01 12:00:00" },
    ]);
    expect(csv).toBe(
      'email,church,size,software,alpha_tester,signed_up\r\n"a@example.org","Grace ""Main""","50-150","\'=HYPERLINK(1)","yes","2026-11-01 12:00:00"\r\n',
    );
  });
});
