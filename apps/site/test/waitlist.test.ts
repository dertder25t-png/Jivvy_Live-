import { describe, expect, it } from "vitest";
import { validateWaitlist } from "../src/lib/waitlist";

describe("validateWaitlist", () => {
  it("accepts and normalizes a good entry", () => {
    const r = validateWaitlist({ email: "  Tech@Church.org ", church: "Grace", size: "100-250", software: "ProPresenter" });
    expect(r).toMatchObject({ ok: true, spam: false, value: { email: "tech@church.org", church: "Grace", alphaTester: false } });
  });
  it("reads the alpha tester checkbox from a form post or JSON", () => {
    for (const alpha of ["on", true, "true", " YES "])
      expect(validateWaitlist({ email: "a@b.co", alpha })).toMatchObject({ ok: true, value: { alphaTester: true } });
    for (const alpha of [undefined, "", "off", false, 1, "no"])
      expect(validateWaitlist({ email: "a@b.co", alpha })).toMatchObject({ ok: true, value: { alphaTester: false } });
  });
  it("rejects bad emails and non-objects", () => {
    for (const x of [null, "x", {}, { email: "nope" }, { email: "a@b" }, { email: 5 }])
      expect(validateWaitlist(x).ok).toBe(false);
  });
  it("flags the honeypot", () => {
    expect(validateWaitlist({ email: "a@b.co", website: "http://spam" })).toMatchObject({ ok: true, spam: true });
  });
  it("truncates oversize fields", () => {
    const r = validateWaitlist({ email: "a@b.co", church: "x".repeat(500) });
    expect(r.ok && r.value.church.length).toBe(120);
  });
});
