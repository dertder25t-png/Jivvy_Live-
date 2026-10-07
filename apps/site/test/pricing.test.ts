import { describe, expect, it } from "vitest";
import { PRICING, plusMonthly } from "../src/lib/pricing";

describe("pricing", () => {
  it("matches the build plan", () => {
    expect(PRICING).toEqual({ oneTime: 200, plusYearly: 60, plusBilledMonthlyPerYear: 80, trialDays: 30 });
    expect(plusMonthly()).toBe("$6.67");
  });
});
