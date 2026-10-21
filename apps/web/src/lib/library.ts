/**
 * The church computer's library (songs and plans for now; themes and settings use the same
 * documents later): the daemon keeps each as a document with a revision, so a save based on
 * an older copy is refused (`conflict`) instead of losing an edit made on another device.
 */
import type { Ack, Command, Doc, DocInfo } from "@jivvy/protocol";
import type { Section, Song } from "@jivvy/songs";
import type { Draft, DraftItem } from "./runsheet";

export interface Sender {
  send(command: Command): Promise<Ack>;
}

export async function listDocs(conn: Sender, kind: string): Promise<DocInfo[]> {
  const ack = await conn.send({ type: "doc.list", kind });
  if (!ack.ok) throw new Error(ack.message);
  return ack.docs ?? [];
}

export async function getDoc(conn: Sender, kind: string, id: string): Promise<Doc> {
  const ack = await conn.send({ type: "doc.get", kind, id });
  if (!ack.ok || !ack.doc) throw new Error(ack.ok ? "The church computer sent nothing back." : ack.message);
  return ack.doc;
}

/** Deletes a song or plan. The daemon keeps a delete marker, so it leaves every list and a save on the old copy is refused. */
export async function deleteDoc(conn: Sender, kind: string, id: string): Promise<void> {
  const ack = await conn.send({ type: "doc.delete", kind, id });
  if (!ack.ok) throw new Error(ack.message);
}

export type Saved = { ok: true; rev: number } | { ok: false; conflict: boolean; message: string };

export async function saveDoc(
  conn: Sender,
  doc: { kind: string; id: string; title: string; body: Record<string, unknown> },
  baseRev?: number,
): Promise<Saved> {
  const ack = await conn.send({ type: "doc.put", ...doc, ...(baseRev === undefined ? {} : { baseRev }) });
  if (ack.ok && ack.saved) return { ok: true, rev: ack.saved.rev };
  return { ok: false, conflict: !ack.ok && ack.code === "conflict", message: ack.ok ? "Not saved." : ack.message };
}

const isStr = (x: unknown): x is string => typeof x === "string";
const isObj = (x: unknown): x is Record<string, unknown> => typeof x === "object" && x !== null && !Array.isArray(x);

/** A saved song's body as a song; anything malformed (another version wrote it) is left out. */
export function songFromDoc(doc: Doc): Song {
  const b = doc.body;
  const sections: Section[] = (Array.isArray(b.sections) ? b.sections : [])
    .filter(isObj)
    .filter((s) => isStr(s.id) && isStr(s.label) && isStr(s.text))
    .map((s) => ({ id: s.id as string, label: s.label as string, text: s.text as string }));
  const arrangement = (Array.isArray(b.arrangement) ? b.arrangement : []).filter(isStr);
  return {
    title: doc.title,
    ...(isStr(b.author) ? { author: b.author } : {}),
    ...(isStr(b.copyright) ? { copyright: b.copyright } : {}),
    sections,
    arrangement,
  };
}

/** A saved plan's body as the run sheet draft; null if it isn't one. */
export function planFromDoc(doc: Doc): Draft | null {
  const items = doc.body.items;
  if (!Array.isArray(items)) return null;
  const ok = items.every((i) => isObj(i) && isStr(i.id) && isStr(i.kind) && isStr(i.title) && isStr(i.text));
  return ok ? { id: doc.id, title: doc.title, items: items as unknown as DraftItem[] } : null;
}

/** An id for a song saved from this browser: from its title, so it reads well, plus a bit of randomness. */
export function newSongId(title: string): string {
  const base = title.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-|-$/g, "").slice(0, 40) || "song";
  return `${base}-${crypto.randomUUID().slice(0, 8)}`;
}
