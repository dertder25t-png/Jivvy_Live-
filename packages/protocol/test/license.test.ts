import { describe, expect, it } from "vitest";
import { signLicenseKey, verifyLicenseKey } from "../src/index";
import fixture from "../fixtures/license.json";

describe("license keys (the same fixture runs in the Rust daemon)", () => {
  it("issues the fixture's key and verifies it", async () => {
    expect(await signLicenseKey(fixture.payload, fixture.testOnlyPrivateKeyPkcs8)).toBe(fixture.key);
    expect(await verifyLicenseKey(fixture.key, fixture.publicKey)).toEqual(fixture.payload);
  });

  it.each(fixture.bad.map((b) => [b.name, b.key] as const))("refuses a key with its %s", async (_name, key) => {
    expect(await verifyLicenseKey(key, fixture.publicKey)).toBeNull();
  });
});
