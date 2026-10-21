import { describe, expect, it } from "vitest";
import { signLicenseKey, verifyLicenseKey, type LicenseStatus } from "../src/index";
import fixture from "../fixtures/license.json";

describe("license keys (the same fixture runs in the Rust daemon)", () => {
  it("issues the fixture's key and verifies it", async () => {
    expect(await signLicenseKey(fixture.payload, fixture.testOnlyPrivateKeyPkcs8)).toBe(fixture.key);
    expect(await verifyLicenseKey(fixture.key, fixture.publicKey)).toEqual(fixture.payload);
  });

  it("the status the daemon sends carries a hint, never the key", async () => {
    const status: LicenseStatus = { ...fixture.status, state: "licensed" };
    expect(status).not.toHaveProperty("key");
    expect(status.keyHint).toBe(`JL1…${fixture.key.slice(-4)}`);
    expect(await verifyLicenseKey(fixture.olderKey, fixture.publicKey)).toMatchObject({ issued: fixture.payload.issued - 86_400_000 });
  });

  it.each(fixture.bad.map((b) => [b.name, b.key] as const))("refuses a key with its %s", async (_name, key) => {
    expect(await verifyLicenseKey(key, fixture.publicKey)).toBeNull();
  });
});
