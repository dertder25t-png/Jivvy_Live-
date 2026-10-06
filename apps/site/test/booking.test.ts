import { describe, expect, it } from "vitest";
import { bookingUrl } from "../src/lib/booking";
import { PRICING, plusMonthly } from "../src/lib/pricing";

describe("bookingUrl", () => {
  it("accepts Google Calendar booking pages", () => {
    expect(bookingUrl("https://calendar.app.google/AbC123xyz")).toBe("https://calendar.app.google/AbC123xyz");
    expect(bookingUrl("  https://calendar.google.com/calendar/appointments/schedules/AcZssZ1  ")).toBe(
      "https://calendar.google.com/calendar/appointments/schedules/AcZssZ1",
    );
  });
  it("returns null when unset", () => {
    for (const x of [undefined, "", "   "]) expect(bookingUrl(x)).toBeNull();
  });
  it("rejects other hosts, other Google pages, http and malformed values", () => {
    for (const x of [
      "http://calendar.app.google/AbC123xyz",
      "https://calendar.app.google/",
      "https://calendar.google.com/calendar/u/0/r",
      "https://calendar.google.com.evil.example/calendar/appointments/x",
      "https://calendly.com/jivvy",
      "javascript:alert(1)",
      "https://user:pw@calendar.app.google/AbC123xyz",
      "calendar.app.google/AbC123xyz",
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
