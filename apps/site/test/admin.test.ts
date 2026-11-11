import { describe, expect, it } from "vitest";
import { SESSION_COOKIE, sha256Hex } from "../src/lib/account";
import { adminEmails, requireAdmin, waitlistCsv } from "../src/lib/admin";

const now = Date.UTC(2026, 10, 10);

// Just enough of D1 for signedIn(): one session row per token hash.
function db(sessions: Record<string, { id: string; email: string }>) {
  return {
    prepare: () => ({
      bind: (hash: string) => ({ first: async () => sessions[hash] ?? null }),
    }),
  } as unknown as D1Database;
}

const asking = (token?: string) =>
  new Request("https://live.example/api/admin/waitlist", { headers: token ? { cookie: `${SESSION_COOKIE}=${token}` } : {} });

describe("admin page", () => {
  it("lets in only signed-in addresses listed in ADMIN_EMAILS", async () => {
    expect([...adminEmails(" Owner@Example.org, second@example.org\nnot-an-email ")]).toEqual(["owner@example.org", "second@example.org"]);
    const DB = db({
      [await sha256Hex("tok-owner")]: { id: "ch_owner", email: "owner@example.org" },
      [await sha256Hex("tok-church")]: { id: "ch_grace", email: "grace@example.org" },
    });
    const env = { DB, ADMIN_EMAILS: "owner@example.org" };
    expect(await requireAdmin(asking("tok-owner"), env, now)).toEqual({ id: "ch_owner", email: "owner@example.org" });
    expect(((await requireAdmin(asking("tok-church"), env, now)) as Response).status).toBe(403);
    expect(((await requireAdmin(asking(), env, now)) as Response).status).toBe(401);
    // No admins configured: closed to everyone.
    expect(((await requireAdmin(asking("tok-owner"), { DB }, now)) as Response).status).toBe(503);
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
