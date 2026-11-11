// The owner's admin page (live.jivvy.org/admin): read the waitlist and church accounts.
// Admins sign in like any church (/account's emailed code); only addresses listed in the
// ADMIN_EMAILS variable get in. Nothing here is needed during a service.
import { normalEmail } from "./account";
import { json, signedIn, type Church, type Env } from "./server";

/** The addresses in ADMIN_EMAILS (comma or whitespace separated), normalised. Empty when unset. */
export function adminEmails(list: string | undefined): Set<string> {
  const out = new Set<string>();
  for (const part of (list ?? "").split(/[\s,]+/)) {
    const email = normalEmail(part);
    if (email) out.add(email);
  }
  return out;
}

/** The signed-in admin, or the response to send instead (503 without DB or admins, 401, 403). */
export async function requireAdmin(request: Request, env: Env, nowMs: number): Promise<Church | Response> {
  const admins = adminEmails(env.ADMIN_EMAILS);
  if (!env.DB || admins.size === 0) return json({ ok: false, error: "The admin page isn't set up." }, 503);
  const church = await signedIn(request, env.DB, nowMs);
  if (!church) return json({ ok: false, signedIn: false, error: "Please sign in." }, 401);
  if (!admins.has(church.email)) return json({ ok: false, signedIn: true, error: "This address isn't an admin." }, 403);
  return church;
}

export interface WaitlistRow {
  email: string;
  church: string;
  size: string;
  software: string;
  alphaTester: boolean;
  createdAt: string;
}

const cell = (v: string) => {
  // Quote every cell, and stop a spreadsheet reading a signup's answer as a formula.
  const safe = /^[=+\-@\t\r]/.test(v) ? `'${v}` : v;
  return `"${safe.replace(/"/g, '""')}"`;
};

/** The waitlist as CSV, for a spreadsheet or an email tool. */
export function waitlistCsv(rows: WaitlistRow[]): string {
  const head = ["email", "church", "size", "software", "alpha_tester", "signed_up"];
  const lines = rows.map((r) => [r.email, r.church, r.size, r.software, r.alphaTester ? "yes" : "no", r.createdAt].map(cell).join(","));
  return [head.join(","), ...lines].join("\r\n") + "\r\n";
}
