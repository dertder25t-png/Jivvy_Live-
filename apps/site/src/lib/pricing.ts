// Pricing shown on live.jivvy.org. Source: "Costs, pricing and business" in docs/BUILD_PLAN.md.
export const PRICING = {
  oneTime: 200,
  plusYearly: 60,
  plusBilledMonthlyPerYear: 80,
  trialDays: 30,
} as const;

/** Monthly charge when Plus is billed monthly, e.g. "$6.67". */
export const plusMonthly = () => `$${(PRICING.plusBilledMonthlyPerYear / 12).toFixed(2)}`;
