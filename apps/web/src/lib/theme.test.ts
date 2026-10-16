import type { Ack, Command } from "@jivvy/protocol";
import { describe, expect, it } from "vitest";
import { getDoc, saveDoc, type Sender } from "./library";
import { ACTIVE_THEME_ID, BUILT_IN, activeBody, activeFromDoc, themeBody, themeFromDoc, themeStyle } from "./theme";

describe("themes", () => {
  it("saves a theme like a song, reads it back, and ignores one it can't read", async () => {
    const store = new Map<string, { title: string; body: Record<string, unknown> }>();
    const conn: Sender = {
      async send(c: Command): Promise<Ack> {
        const base = { v: 1, id: "x" } as const;
        if (c.type === "doc.put") {
          store.set(`${c.kind}/${c.id}`, { title: c.title, body: c.body });
          return { ...base, ok: true, saved: { kind: c.kind, id: c.id, title: c.title, rev: 1, updatedAt: 1 } };
        }
        if (c.type === "doc.get") {
          const d = store.get(`${c.kind}/${c.id}`)!;
          return { ...base, ok: true, doc: { kind: c.kind, id: c.id, title: d.title, rev: 1, updatedAt: 1, body: d.body } };
        }
        return { ...base, ok: false, code: "unknown_command", message: c.type };
      },
    };
    const theme = { ...BUILT_IN[0]!.theme, size: 8, background: { kind: "image" as const, image: "abc", color: "#10224F" } };
    await saveDoc(conn, { kind: "theme", id: "mine", title: "Mine", body: themeBody(theme) });
    expect(themeFromDoc(await getDoc(conn, "theme", "mine"))).toEqual(theme);
    expect(themeStyle(theme)).toContain("--t-size:4.5cqw");

    const doc = { kind: "theme", id: "n", title: "", rev: 1, updatedAt: 1 };
    expect(themeFromDoc({ ...doc, body: { ...themeBody(theme), v: 2 } })).toBeNull();
    expect(themeFromDoc({ ...doc, body: { ...themeBody(theme), text: "red" } })).toBeNull();
    expect(themeFromDoc({ ...doc, body: { ...themeBody(theme), bold: "yes" } })).toBeNull();
    expect(themeFromDoc({ ...doc, body: { ...themeBody(theme), shadow: 1 } })).toBeNull();
    // The theme in use is a setting every device reads.
    await saveDoc(conn, { kind: "setting", id: ACTIVE_THEME_ID, title: "Active theme", body: activeBody("mine") });
    expect(activeFromDoc(await getDoc(conn, "setting", ACTIVE_THEME_ID))).toBe("mine");
    expect(activeFromDoc({ ...doc, body: { v: 2, themeId: "x" } })).toBeNull();
    for (const b of BUILT_IN) expect(themeFromDoc({ ...doc, body: themeBody(b.theme) })).toEqual(b.theme);
  });
});
