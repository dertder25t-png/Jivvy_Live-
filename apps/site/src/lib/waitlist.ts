export interface WaitlistEntry {
  email: string;
  church: string;
  size: string;
  software: string;
}

export type WaitlistResult =
  | { ok: true; spam: boolean; value: WaitlistEntry }
  | { ok: false; error: string };

const EMAIL = /^[^\s@]+@[^\s@]+\.[^\s@]{2,}$/;
const clean = (x: unknown, max: number) => (typeof x === "string" ? x.trim().slice(0, max) : "");

export function validateWaitlist(input: unknown): WaitlistResult {
  const d = (typeof input === "object" && input !== null ? input : {}) as Record<string, unknown>;
  const email = clean(d.email, 254).toLowerCase();
  if (!EMAIL.test(email)) return { ok: false, error: "Please enter a valid email address." };
  return {
    ok: true,
    spam: clean(d.website, 100) !== "",
    value: { email, church: clean(d.church, 120), size: clean(d.size, 40), software: clean(d.software, 80) },
  };
}
