import { describe, expect, it } from "vitest";
import { bookingUrl } from "../src/lib/booking";
import { PRICING, plusMonthly } from "../src/lib/pricing";

describe("bookingUrl", () => {
  it("accepts https Calendly links", () => {
    expect(bookingUrl("https://calendly.com/jivvy/intro")).toBe("https://calendly.com/jivvy/intro");
    expect(bookingUrl("  https://calendly.com/jivvy  ")).toBe("https://calendly.com/jivvy");
  });
  it("returns null when unset", () => {
    for (const x of [undefined, "", "   "]) expect(bookingUrl(x)).toBeNull();
  });
  it("rejects other hosts, http and malformed values", () => {
    for (const x of [
      "http://calendly.com/jivvy",
      "https://calendly.com.evil.example/jivvy",
      "https://evilcalendly.com/jivvy",
      "javascript:alert(1)",
      "https://user:pw@calendly.com/jivvy",
      "calendly.com/jivvy",
    ])
      expect(bookingUrl(x)).toBeNull();
  });
});

describe("pricing", () => {
  it("matches the build plan", () => {
    expect(PRICING).toEqual({ oneTime: 200, plusYearly: 60, plusBilledMonthlyPerYear: 80, trialDays: 30 });
    expect(plusMonthly()).toBe("$6.67");
  });
});
