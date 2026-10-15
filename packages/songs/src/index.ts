/**
 * Songs as Jivvy Live keeps them: named sections (Verse 1, Chorus, Bridge…) and an arrangement,
 * the order they are sung in, where a section can come back any number of times. Pasted lyrics
 * become sections by themselves. No runtime dependencies: the web app uses it today, importers
 * and the church computer's library later.
 */

export interface Section {
  /** Unique within its song; the arrangement refers to it. */
  id: string;
  /** "Verse 1", "Chorus", "Bridge"… for the operator; never on the screens. */
  label: string;
  /** The words. A blank line starts a new slide. */
  text: string;
}

export interface Song {
  title: string;
  author?: string;
  /** Who holds the words, e.g. "Public domain". */
  copyright?: string;
  sections: Section[];
  /** Section ids in the order they are sung. */
  arrangement: string[];
}

/** Section names a header line may use, and the label each becomes. */
const NAMES: Record<string, string> = {
  verse: "Verse",
  vs: "Verse",
  v: "Verse",
  chorus: "Chorus",
  ch: "Chorus",
  c: "Chorus",
  refrain: "Refrain",
  prechorus: "Pre-Chorus",
  pc: "Pre-Chorus",
  bridge: "Bridge",
  b: "Bridge",
  tag: "Tag",
  intro: "Intro",
  outro: "Outro",
  ending: "Ending",
  interlude: "Interlude",
  instrumental: "Instrumental",
  vamp: "Vamp",
  coda: "Coda",
};

/** The kinds of section the editor offers to add. */
export const SECTION_NAMES = ["Verse", "Chorus", "Pre-Chorus", "Bridge", "Refrain", "Tag", "Intro", "Outro", "Ending"];

/**
 * A line that names a section rather than being sung: "Verse 1", "[Chorus]", "CHORUS:",
 * "V2", "Pre-Chorus", "Chorus x2", "(Repeat Chorus)". Null for a lyric line.
 */
export function sectionHeader(line: string): { label: string; repeat: number } | null {
  let s = line.trim().toLowerCase();
  if (!s || s.length > 32) return null;
  let repeat = 1;
  // "x2", "(x2)", "2x" at the end: sung that many times.
  const times = /\s*[([]?\s*(?:[x×]\s*(\d)|(\d)\s*[x×])\s*[)\]]?$/.exec(s);
  if (times && times.index > 0) {
    repeat = Number(times[1] ?? times[2]);
    s = s.slice(0, times.index);
  }
  const marked = /^[[(]|[\]):]$/.test(s);
  s = s
    .replace(/^[[(]\s*/, "")
    .replace(/\s*[\])]$/, "")
    .replace(/\s*:$/, "")
    .replace(/^repeat\s+(?:the\s+)?/, "")
    .trim();
  const m = /^([a-z][a-z -]*?)\.?\s*(\d{1,2})?$/.exec(s);
  const key = m?.[1]?.replace(/[\s-]+/g, "") ?? "";
  const name = NAMES[key];
  if (!m || !name || repeat < 1) return null;
  // A lone letter ("C", "b") is a lyric unless numbered or marked: "C2", "[C]", "C:".
  if (key.length === 1 && !m[2] && !marked) return null;
  return { label: m[2] ? `${name} ${Number(m[2])}` : name, repeat };
}

/** Lines with runs of blank lines made one, and none at either end. */
function tidy(lines: string[]): string {
  return lines
    .join("\n")
    .replace(/\n{3,}/g, "\n\n")
    .replace(/^\n+|\n+$/g, "");
}

/** The same words, whatever the spacing and punctuation: how a repeated chorus is spotted. */
const sameWords = (text: string) =>
  text
    .toLowerCase()
    .replace(/[^\p{L}\p{N}]+/gu, " ")
    .trim();

const isChorus = (label: string) => /^(Chorus|Refrain)\b/.test(label);
const isVerse = (label: string) => /^Verse\b/.test(label);

/**
 * Sections and arrangement from pasted lyrics.
 * - A header line ("Verse 1", "[Chorus]", "C:") starts a section; everything up to the next
 *   header belongs to it, blank lines included (they start new slides).
 * - "1. Amazing grace…" (a numbered hymn stanza) starts Verse 1.
 * - A header with no words after it repeats the section of that name ("Chorus" again, "Chorus x2").
 * - With no headers, each paragraph is a section; one that comes back word for word is the Chorus.
 * - A chorus or refrain written once, right after the first verse (how hymnals print it), is
 *   sung after every verse.
 */
export function parseLyrics(text: string): Pick<Song, "sections" | "arrangement"> {
  type Block = { label: string | null; repeat: number; lines: string[] };
  const blocks: Block[] = [];
  let open: Block | null = null;
  for (const raw of text.replace(/\r\n?/g, "\n").split("\n")) {
    const line = raw.replace(/[\u0000-\u0008\u000b-\u001f\u007f]+/g, " ").trim();
    const header = sectionHeader(line);
    const stanza = header ? null : /^(\d{1,2})[.)]\s+(\S.*)$/.exec(line);
    if (header) {
      open = { label: header.label, repeat: header.repeat, lines: [] };
      blocks.push(open);
    } else if (stanza) {
      open = { label: `Verse ${Number(stanza[1])}`, repeat: 1, lines: [stanza[2] ?? ""] };
      blocks.push(open);
    } else if (!line) {
      // A blank line ends a paragraph that has no header; inside a named section it's a new slide.
      if (open?.label === null) open = null;
      else open?.lines.push("");
    } else {
      if (!open) {
        open = { label: null, repeat: 1, lines: [] };
        blocks.push(open);
      }
      open.lines.push(line);
    }
  }

  const unnamedSeen = new Map<string, number>();
  for (const b of blocks) {
    const key = sameWords(tidy(b.lines));
    if (b.label === null) unnamedSeen.set(key, (unnamedSeen.get(key) ?? 0) + 1);
  }

  const sections: Section[] = [];
  const arrangement: string[] = [];
  let verses = 0;
  const labelled = (label: string) => sections.find((s) => s.label.toLowerCase() === label.toLowerCase());
  for (const b of blocks) {
    const words = tidy(b.lines);
    let section: Section | undefined;
    if (!words) {
      // "Chorus" again with nothing under it: sing that one again ("Chorus" also finds "Chorus 1").
      if (b.label === null) continue;
      section = labelled(b.label) ?? labelled(`${b.label} 1`);
      if (!section) continue;
    } else {
      section = sections.find((s) => sameWords(s.text) === sameWords(words));
      if (section && b.label !== null && section.label !== b.label && !isChorus(b.label)) section = undefined;
    }
    if (!section) {
      let label = b.label ?? ((unnamedSeen.get(sameWords(words)) ?? 0) > 1 ? "Chorus" : "Verse");
      if (label === "Verse") label = `Verse ${++verses}`;
      else if (isVerse(label)) verses = Math.max(verses, Number(label.slice(6)) || 0);
      // Two different choruses: the second is "Chorus 2".
      if (labelled(label)) {
        let n = 2;
        while (labelled(`${label} ${n}`)) n++;
        label = `${label} ${n}`;
      }
      section = { id: uniqueId(sections, label), label, text: words };
      sections.push(section);
    }
    for (let i = 0; i < b.repeat; i++) arrangement.push(section.id);
  }
  return { sections, arrangement: refrainAfterEachVerse(sections, arrangement) };
}

/** A song from a title and pasted lyrics. */
export function songFromLyrics(title: string, lyrics: string, extra: Partial<Song> = {}): Song {
  return { title, ...extra, ...parseLyrics(lyrics) };
}

function refrainAfterEachVerse(sections: Section[], arrangement: string[]): string[] {
  const choruses = sections.filter((s) => isChorus(s.label));
  const chorus = choruses[0];
  if (choruses.length !== 1 || !chorus || arrangement.filter((id) => id === chorus.id).length !== 1) return arrangement;
  const label = (id: string) => sections.find((s) => s.id === id)?.label ?? "";
  const first = arrangement.indexOf(chorus.id);
  // Only the hymnal pattern: the refrain right after the first verse, and nothing before that verse.
  if (first !== 1 || !isVerse(label(arrangement[0] ?? ""))) return arrangement;
  return arrangement.flatMap((id, i) =>
    i > first && isVerse(label(id)) && arrangement[i + 1] !== chorus.id ? [id, chorus.id] : [id],
  );
}

/** An id for a new section, from its label: "Verse 2" → "verse-2". */
export function uniqueId(sections: Section[], label: string): string {
  const base =
    label
      .toLowerCase()
      .replace(/[^a-z0-9]+/g, "-")
      .replace(/^-|-$/g, "") || "section";
  let id = base;
  for (let n = 2; sections.some((s) => s.id === id); n++) id = `${base}-${n}`;
  return id;
}

/** The label a new section of this kind gets: the next free "Verse 3", or "Chorus 2". */
export function nextLabel(song: Song, name: string): string {
  const taken = (label: string) => song.sections.some((s) => s.label.toLowerCase() === label.toLowerCase());
  if (name === "Verse") {
    let n = 1;
    while (taken(`Verse ${n}`)) n++;
    return `Verse ${n}`;
  }
  if (!taken(name)) return name;
  let n = 2;
  while (taken(`${name} ${n}`)) n++;
  return `${name} ${n}`;
}

/** The sections in the order they are sung; ids no longer in the song are skipped. */
export function sung(song: Song): Section[] {
  return song.arrangement.flatMap((id) => song.sections.filter((s) => s.id === id));
}

/**
 * Lines shared out over as few slides as fit `max` lines each, as evenly as possible: six lines
 * are two slides of three, not four and two.
 */
export function balance<T>(lines: T[], max: number): T[][] {
  const count = Math.ceil(lines.length / Math.max(1, max));
  const out: T[][] = [];
  let start = 0;
  for (let i = 0; i < count; i++) {
    const size = Math.ceil((lines.length - start) / (count - i));
    out.push(lines.slice(start, start + size));
    start += size;
  }
  return out;
}
