/**
 * Export of everything in the church computer's library in open formats: one .zip with a JSON
 * file per document, plus plain text for songs and run sheets. Offline, on every license.
 * Stream keys and anything else secret are never written (see `scrub`).
 */
import type { Doc } from "@jivvy/protocol";
import type { Song } from "@jivvy/songs";
import { getDoc, listDocs, planFromDoc, songFromDoc, type Sender } from "./library";

/** Kinds exported. */
export const KINDS = ["song", "plan", "theme", "setting"];

/** Settings documents that hold devices or the stream: never exported at all. */
const SECRET_DOCS = new Set(["stream", "devices"]);
/** Field names never exported (compared lower-case, without - _ or spaces). */
const SECRET_NAMES = new Set(
  ["streamKey", "apiKey", "secretKey", "accessKey", "privateKey", "secret", "clientSecret", "token", "accessToken", "refreshToken",
   "password", "passcode", "passphrase", "pin", "authorization", "credentials"].map((n) => n.toLowerCase()),
);
const isSecretName = (k: string) => SECRET_NAMES.has(k.toLowerCase().replace(/[-_ ]/g, ""));
/** A stream address that carries its key in the path. */
const STREAM_URL = /^\s*(rtmps?|srt):\/\//i;

const isObj = (x: unknown): x is Record<string, unknown> => typeof x === "object" && x !== null && !Array.isArray(x);

/** A copy of `value` without any field whose name looks like a key, secret or token. */
export function scrub(value: unknown, kind = ""): unknown {
  if (Array.isArray(value)) return value.map((v) => scrub(v, kind));
  if (typeof value === "string" && kind === "setting" && STREAM_URL.test(value)) return "";
  if (!isObj(value)) return value;
  const out: Record<string, unknown> = {};
  for (const [k, v] of Object.entries(value)) {
    if (isSecretName(k) || (kind === "setting" && /^key$/i.test(k))) continue;
    out[k] = scrub(v, kind);
  }
  return out;
}

export interface ExportFile {
  path: string;
  data: string | Uint8Array;
}

/** A song as plain text that opens anywhere. */
export function songText(song: Song): string {
  const out = [song.title];
  if (song.author) out.push(`by ${song.author}`);
  if (song.copyright) out.push(song.copyright);
  out.push("");
  for (const s of song.sections) out.push(s.label, s.text.trim(), "");
  const label = (id: string) => song.sections.find((s) => s.id === id)?.label ?? id;
  out.push(`Order: ${song.arrangement.map(label).join(", ")}`);
  return out.join("\n") + "\n";
}

/** A run sheet as a readable order of service. */
export function planText(title: string, items: { kind: string; title: string; text: string }[]): string {
  const out = [title, "=".repeat(Math.max(3, title.length)), ""];
  items.forEach((it, i) => {
    out.push(`${i + 1}. ${it.title || it.kind} (${it.kind})`);
    if (it.text.trim()) out.push(...it.text.trim().split("\n").map((l) => `   ${l}`));
  });
  return out.join("\n") + "\n";
}

const RESERVED = /^(con|prn|aux|nul|com[1-9]|lpt[1-9])$/;
/** A file name part: letters and digits in any script; Windows reserved names get a suffix. */
const slug = (s: string) => {
  const t = s.toLowerCase().replace(/[^\p{L}\p{N}]+/gu, "-").replace(/^-|-$/g, "").slice(0, 60) || "untitled";
  return RESERVED.test(t) ? `${t}-file` : t;
};

const README = `Jivvy Live export
=================

Everything here is plain text or JSON. Nothing needs Jivvy Live to open it.

json/<kind>/<id>.json   One file per document exactly as stored:
                        { "kind", "id", "title", "rev", "updatedAt", "body" }.
                        kind is song, plan, theme or setting. "rev" is its revision.
songs/<title>.txt       Each song as text: title, author, sections with their names,
                        and the Order line (the arrangement, in the order sung).
runsheets/<title>.txt   Each run sheet as an order of service.

Not included: stream keys, passwords, tokens and device settings. Fields with such names
are removed from every file. Pictures and PDFs are not part of this export.
`;

/** The files of the export for these documents. */
export function exportFiles(docs: Doc[]): ExportFile[] {
  const files: ExportFile[] = [{ path: "README.txt", data: README }];
  const used = new Set<string>();
  const unique = (p: string) => {
    let n = p;
    let i = 2;
    while (used.has(n)) n = p.replace(/(\.[a-z]+)$/, `-${i++}$1`);
    used.add(n);
    return n;
  };
  for (const d of docs) {
    if (d.kind === "setting" && SECRET_DOCS.has(d.id)) continue;
    const body = scrub(d.body, d.kind) as Record<string, unknown>;
    const json = JSON.stringify({ kind: d.kind, id: d.id, title: d.title, rev: d.rev, updatedAt: d.updatedAt, body }, null, 2);
    files.push({ path: unique(`json/${slug(d.kind)}/${slug(d.id)}.json`), data: json + "\n" });
    const safe = { ...d, body };
    if (d.kind === "song") files.push({ path: unique(`songs/${slug(d.title)}.txt`), data: songText(songFromDoc(safe)) });
    if (d.kind === "plan") {
      const plan = planFromDoc(safe);
      if (plan) files.push({ path: unique(`runsheets/${slug(d.title)}.txt`), data: planText(plan.title, plan.items) });
    }
  }
  return files;
}

/** Every document of every kind, read from the church computer. */
export async function readAll(conn: Sender, kinds = KINDS): Promise<{ docs: Doc[]; skipped: number }> {
  const docs: Doc[] = [];
  let skipped = 0;
  for (const kind of kinds)
    for (const info of await listDocs(conn, kind)) {
      try {
        docs.push(await getDoc(conn, kind, info.id));
      } catch {
        skipped++;
      }
    }
  return { docs, skipped };
}

// --- A zip with no compression ("store"), so no dependency is needed. ---

const CRC = (() => {
  const t = new Uint32Array(256);
  for (let n = 0; n < 256; n++) {
    let c = n;
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    t[n] = c >>> 0;
  }
  return t;
})();

export function crc32(b: Uint8Array): number {
  let c = 0xffffffff;
  for (const x of b) c = CRC[(c ^ x) & 255]! ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
}

export function zip(files: ExportFile[], now = new Date()): Uint8Array {
  const enc = new TextEncoder();
  const time = (now.getHours() << 11) | (now.getMinutes() << 5) | (now.getSeconds() >> 1);
  const date = ((Math.max(now.getFullYear(), 1980) - 1980) << 9) | ((now.getMonth() + 1) << 5) | now.getDate();
  const parts: Uint8Array[] = [];
  const central: Uint8Array[] = [];
  let offset = 0;
  for (const f of files) {
    const name = enc.encode(f.path);
    const data = typeof f.data === "string" ? enc.encode(f.data) : f.data;
    const crc = crc32(data);
    const h = new Uint8Array(30 + name.length);
    const v = new DataView(h.buffer);
    v.setUint32(0, 0x04034b50, true);
    v.setUint16(4, 20, true);
    v.setUint16(6, 0x0800, true); // names are UTF-8
    v.setUint16(10, time, true);
    v.setUint16(12, date, true);
    v.setUint32(14, crc, true);
    v.setUint32(18, data.length, true);
    v.setUint32(22, data.length, true);
    v.setUint16(26, name.length, true);
    h.set(name, 30);
    const c = new Uint8Array(46 + name.length);
    const cv = new DataView(c.buffer);
    cv.setUint32(0, 0x02014b50, true);
    cv.setUint16(4, 20, true);
    cv.setUint16(6, 20, true);
    cv.setUint16(8, 0x0800, true);
    cv.setUint16(12, time, true);
    cv.setUint16(14, date, true);
    cv.setUint32(16, crc, true);
    cv.setUint32(20, data.length, true);
    cv.setUint32(24, data.length, true);
    cv.setUint16(28, name.length, true);
    cv.setUint32(42, offset, true);
    c.set(name, 46);
    parts.push(h, data);
    central.push(c);
    offset += h.length + data.length;
  }
  const size = central.reduce((n, c) => n + c.length, 0);
  const end = new Uint8Array(22);
  const ev = new DataView(end.buffer);
  ev.setUint32(0, 0x06054b50, true);
  ev.setUint16(8, files.length, true);
  ev.setUint16(10, files.length, true);
  ev.setUint32(12, size, true);
  ev.setUint32(16, offset, true);
  const all = [...parts, ...central, end];
  const out = new Uint8Array(all.reduce((n, p) => n + p.length, 0));
  let o = 0;
  for (const p of all) {
    out.set(p, o);
    o += p.length;
  }
  return out;
}

/** The names in a zip made by `zip` (for tests). */
export function zipNames(z: Uint8Array): string[] {
  const v = new DataView(z.buffer, z.byteOffset, z.byteLength);
  const n = v.getUint16(z.length - 22 + 10, true);
  let o = v.getUint32(z.length - 22 + 16, true);
  const names: string[] = [];
  for (let i = 0; i < n; i++) {
    const len = v.getUint16(o + 28, true);
    names.push(new TextDecoder().decode(z.subarray(o + 46, o + 46 + len)));
    o += 46 + len;
  }
  return names;
}
