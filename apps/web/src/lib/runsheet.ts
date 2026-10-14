/**
 * The run sheet as the operator edits it, and how each item becomes slides for the daemon.
 * The daemon only ever sees slides (packages/protocol `RunSheet`); the words typed here stay
 * in the browser until the local database item.
 */
import { MAX_SLIDE_LINE, MAX_SLIDE_LINES, MAX_TITLE, type RunSheet, type Slide } from "@jivvy/protocol";

export type ItemKind = "song" | "scripture" | "countdown" | "text";

export interface DraftItem {
  id: string;
  kind: ItemKind;
  title: string;
  /** Song lyrics, scripture text or announcement text. Unused by countdowns. */
  text: string;
  /** Countdowns: the time of day it counts down to, "HH:MM" (24-hour). */
  at?: string;
}

export interface Draft {
  id: string;
  title: string;
  items: DraftItem[];
}

/** Most lines on one lyric slide; longer verses are split across slides. */
export const SONG_LINES = 4;
/** Scripture is wrapped to about this many characters a line, this many lines a slide. */
export const SCRIPTURE_WIDTH = 42;
export const SCRIPTURE_LINES = 4;

export const newId = (): string => crypto.randomUUID();

export function newItem(kind: ItemKind): DraftItem {
  const title = { song: "New song", scripture: "", countdown: "Countdown", text: "Announcement" }[kind];
  return kind === "countdown"
    ? { id: newId(), kind, title, text: "Service starts in", at: "10:30" }
    : { id: newId(), kind, title, text: "" };
}

/** One line as the protocol accepts it: no control characters, not too long. */
function clean(line: string, max = MAX_SLIDE_LINE): string {
  const plain = line.replace(/[\u0000-\u001f\u007f]+/g, " ").trim();
  return [...plain].slice(0, max).join("");
}

/** Paragraphs (separated by blank lines), each a list of non-empty lines, not yet shortened. */
function paragraphs(text: string): string[][] {
  return text
    .split(/\r?\n\s*\r?\n/)
    .map((p) => p.split(/\r?\n/).map((l) => clean(l, Infinity)).filter(Boolean))
    .filter((p) => p.length > 0);
}

function chunks<T>(list: T[], size: number): T[][] {
  const out: T[][] = [];
  for (let i = 0; i < list.length; i += size) out.push(list.slice(i, i + size));
  return out;
}

/** Words wrapped into lines of at most `width` characters (a longer word gets its own line). */
export function wrap(text: string, width: number): string[] {
  const lines: string[] = [];
  let line = "";
  for (const word of clean(text.replace(/\s+/g, " "), Infinity).split(" ").filter(Boolean)) {
    if (line && line.length + 1 + word.length > width) {
      lines.push(line);
      line = word;
    } else {
      line = line ? `${line} ${word}` : word;
    }
  }
  if (line) lines.push(line);
  return lines.map((l) => clean(l));
}

/** The next time the clock reads `at` ("HH:MM"), from `now`; null if `at` isn't a time. */
export function nextTime(at: string, now: Date): number | null {
  const m = /^(\d{1,2}):(\d{2})$/.exec(at.trim());
  if (!m) return null;
  const [h, min] = [Number(m[1]), Number(m[2])];
  if (h > 23 || min > 59) return null;
  const t = new Date(now);
  t.setHours(h, min, 0, 0);
  if (t.getTime() <= now.getTime()) t.setDate(t.getDate() + 1);
  return t.getTime();
}

/** The slides one item shows. */
export function slidesFor(item: DraftItem, now = new Date()): Slide[] {
  switch (item.kind) {
    case "song":
    case "text": {
      const size = item.kind === "song" ? SONG_LINES : MAX_SLIDE_LINES;
      return paragraphs(item.text)
        .flatMap((p) => chunks(p, size))
        .map((lines) => ({ lines: lines.map((l) => clean(l)) }));
    }
    case "scripture": {
      // Verses run on; the reference goes under every slide so latecomers know where we are.
      const words = paragraphs(item.text).map((p) => p.join(" ")).join(" ");
      const reference = clean(item.title);
      return chunks(wrap(words, SCRIPTURE_WIDTH), SCRIPTURE_LINES).map((lines) => ({
        lines: reference ? [...lines, reference] : lines,
      }));
    }
    case "countdown": {
      const to = nextTime(item.at ?? "", now);
      const lines = [clean(item.text)].filter(Boolean);
      return [to === null ? { lines } : { lines, countdownTo: to }];
    }
  }
}

/** The run sheet to send to the daemon. */
export function build(draft: Draft, now = new Date()): RunSheet {
  return {
    id: draft.id,
    title: clean(draft.title, MAX_TITLE),
    items: draft.items.map((item) => ({
      id: item.id,
      kind: item.kind,
      title: clean(item.title, MAX_TITLE),
      slides: slidesFor(item, now),
    })),
  };
}
