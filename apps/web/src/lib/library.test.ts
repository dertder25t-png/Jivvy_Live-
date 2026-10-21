import type { Ack, Command } from "@jivvy/protocol";
import { describe, expect, it } from "vitest";
import { deleteDoc, getDoc, listDocs, planFromDoc, saveDoc, songFromDoc, type Sender } from "./library";

/** A church computer's library in memory, with the daemon's revision rule. */
function fakeDaemon(): Sender {
  const docs = new Map<string, { title: string; body: Record<string, unknown>; rev: number }>();
  return {
    async send(c: Command): Promise<Ack> {
      const base = { v: 1, id: "x" } as const;
      if (c.type === "doc.put") {
        const key = `${c.kind}/${c.id}`;
        const rev = docs.get(key)?.rev ?? 0;
        if (c.baseRev !== undefined && c.baseRev !== rev) return { ...base, ok: false, code: "conflict", message: "changed" };
        docs.set(key, { title: c.title, body: c.body, rev: rev + 1 });
        return { ...base, ok: true, saved: { kind: c.kind, id: c.id, title: c.title, rev: rev + 1, updatedAt: 1 } };
      }
      if (c.type === "doc.get") {
        const d = docs.get(`${c.kind}/${c.id}`);
        if (!d) return { ...base, ok: false, code: "bad_arguments", message: "none" };
        return { ...base, ok: true, doc: { kind: c.kind, id: c.id, title: d.title, rev: d.rev, updatedAt: 1, body: d.body } };
      }
      if (c.type === "doc.list") {
        const docs_ = [...docs].filter(([k]) => k.startsWith(`${c.kind}/`)).map(([k, d]) => ({
          kind: c.kind, id: k.slice(c.kind.length + 1), title: d.title, rev: d.rev, updatedAt: 1,
        }));
        return { ...base, ok: true, docs: docs_ };
      }
      if (c.type === "doc.delete") {
        const key = `${c.kind}/${c.id}`;
        const d = docs.get(key);
        if (!d) return { ...base, ok: false, code: "bad_arguments", message: "none" };
        docs.delete(key);
        return { ...base, ok: true };
      }
      return { ...base, ok: false, code: "unknown_command", message: c.type };
    },
  };
}

describe("library", () => {
  it("saves a song and a plan and reads them back, and reports an edit from an old copy as a conflict", async () => {
    const conn = fakeDaemon();
    const song = { title: "Grace", sections: [{ id: "v1", label: "Verse 1", text: "Amazing grace" }], arrangement: ["v1", "v1"] };
    expect(await saveDoc(conn, { kind: "song", id: "grace", title: "Grace", body: song }, 0)).toEqual({ ok: true, rev: 1 });
    expect(songFromDoc(await getDoc(conn, "song", "grace"))).toEqual(song);
    // Another device saved revision 2 meanwhile; this one still has revision 1.
    await saveDoc(conn, { kind: "song", id: "grace", title: "Grace", body: song }, 1);
    const late = await saveDoc(conn, { kind: "song", id: "grace", title: "Grace", body: song }, 1);
    expect(late).toEqual({ ok: false, conflict: true, message: "changed" });

    const items = [{ id: "i", kind: "text", title: "Welcome", text: "Hello" }];
    await saveDoc(conn, { kind: "plan", id: "sun", title: "Sunday", body: { items } });
    expect(planFromDoc(await getDoc(conn, "plan", "sun"))).toEqual({ id: "sun", title: "Sunday", items });
    // A body this version can't read is left alone rather than half-loaded.
    expect(planFromDoc({ kind: "plan", id: "x", title: "", rev: 1, updatedAt: 1, body: { items: [{ id: 1 }] } })).toBeNull();
  });

  it("deleting a song takes it out of the library list", async () => {
    const conn = fakeDaemon();
    const song = { title: "Amazing Grace", sections: [{ id: "v1", label: "Verse 1", text: "Amazing grace" }], arrangement: ["v1"] };
    await saveDoc(conn, { kind: "song", id: "amazing-grace", title: "Amazing Grace", body: song });
    expect((await listDocs(conn, "song")).map((d) => d.id)).toEqual(["amazing-grace"]);
    await deleteDoc(conn, "song", "amazing-grace");
    expect(await listDocs(conn, "song")).toEqual([]);
  });
});
