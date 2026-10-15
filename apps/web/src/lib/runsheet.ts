/**
 * The run sheet as the operator edits it, and how each item becomes slides for the daemon.
 * The daemon only ever sees slides (packages/protocol `RunSheet`); the words typed here stay
 * in the browser until the local database item.
 */
import {
  MAX_ADVANCE_SECONDS,
  MAX_SLIDE_LABEL,
  MAX_SLIDE_LINE,
  MAX_SLIDE_LINES,
  MAX_TITLE,
  type RunSheet,
  type RunSheetItem,
  type Slide,
} from "@jivvy/protocol";
import { balance, songFromLyrics, sung, type Song } from "@jivvy/songs";

export type ItemKind = "song" | "scripture" | "countdown" | "text" | "loop" | "image" | "pdf";

export interface DraftItem {
  id: string;
  kind: ItemKind;
  title: string;
  /** Scripture text or announcement text; a song's lyrics from before the song editor. */
  text: string;
  /** Songs: sections and the order they are sung in. */
  song?: Song;
  /** Songs saved to the church computer's library: its id there and the revision this copy is from. */
  libraryId?: string;
  libraryRev?: number;
  /** Countdowns: the time of day it counts down to, "HH:MM" (24-hour). */
  at?: string;
  /** Pre-service loops: seconds on each slide before the next. */
  seconds?: number;
  /** Images and PDF slides: one slide per image id (kept in this browser, see media.ts). */
  images?: string[];
  /** PDF slides: the title last taken from the file's name (a title typed by hand is kept). */
  titleFrom?: string;
}

export interface Draft {
  id: string;
  title: string;
  items: DraftItem[];
}

/** How long a pre-service loop shows each slide unless the operator says otherwise. */
export const LOOP_SECONDS = 8;

/** Most lines on one lyric slide; longer verses are split across slides. */
export const SONG_LINES = 4;
/** Scripture is wrapped to about this many characters a line, this many lines a slide. */
export const SCRIPTURE_WIDTH = 42;
export const SCRIPTURE_LINES = 4;

export const newId = (): string => crypto.randomUUID();

export function newItem(kind: ItemKind): DraftItem {
  const title = {
    song: "New song",
    scripture: "",
    countdown: "Countdown",
    text: "Announcement",
    loop: "Before the service",
    image: "Images",
    pdf: "PDF slides",
  }[kind];
  if (kind === "image" || kind === "pdf") return { id: newId(), kind, title, text: "", images: [] };
  if (kind === "countdown") return { id: newId(), kind, title, text: "Service starts in", at: "10:30" };
  if (kind === "loop") return { id: newId(), kind, title, text: "", seconds: LOOP_SECONDS };
  if (kind === "song") return { id: newId(), kind, title, text: "", song: { title, sections: [], arrangement: [] } };
  return { id: newId(), kind, title, text: "" };
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

/** A line longer than the protocol allows is wrapped at about this many characters. */
export const LONG_LINE_WIDTH = 60;

/** Words wrapped into lines of at most `width` characters; a longer word is split. */
export function wrap(text: string, width: number): string[] {
  const lines: string[] = [];
  let line = "";
  const words = clean(text.replace(/\s+/g, " "), Infinity).split(" ").filter(Boolean);
  for (const word of words.flatMap((w) => chunks([...w], width).map((c) => c.join("")))) {
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

/** A song item's song; one saved before the song editor gets its sections from its lyrics. */
export const songOf = (item: DraftItem): Song => item.song ?? songFromLyrics(item.title, item.text ?? "");

/** A line too long for a slide (a pasted paragraph) is wrapped, never cut short. */
const fit = (l: string) => ([...l].length > MAX_SLIDE_LINE ? wrap(l, LONG_LINE_WIDTH) : [l]);

/** The slides one item shows. */
export function slidesFor(item: DraftItem, now = new Date()): Slide[] {
  switch (item.kind) {
    case "scripture": {
      // Verses run on; the reference goes under every slide so latecomers know where we are.
      const words = paragraphs(item.text).map((p) => p.join(" ")).join(" ");
      const reference = clean(item.title);
      return chunks(wrap(words, SCRIPTURE_WIDTH), SCRIPTURE_LINES).map((lines) => ({
        lines: reference ? [...lines, reference] : lines,
      }));
    }
    case "image":
    case "pdf": // each page already drawn as a picture in this browser (pdf.ts)
      return (item.images ?? []).map((image) => ({ lines: [], image }));
    case "countdown": {
      const to = nextTime(item.at ?? "", now);
      const lines = [clean(item.text)].filter(Boolean);
      return [to === null ? { lines } : { lines, countdownTo: to }];
    }
    case "song":
      // Each section in the order it is sung, its lines shared evenly over its slides.
      // Each slide carries its section's name for the operator's list (never shown on screen).
      return sung(songOf(item)).flatMap((section) => {
        const label = clean(section.label, MAX_SLIDE_LABEL);
        return paragraphs(section.text)
          .map((p) => p.flatMap(fit))
          .flatMap((p) => balance(p, SONG_LINES))
          .map((lines) => ({ lines: lines.map((l) => clean(l)), ...(label ? { label } : {}) }));
      });
    // Text and pre-service loops; also any kind this version doesn't know (a draft saved by a newer one).
    default:
      return paragraphs(item.text ?? "")
        .map((p) => p.flatMap(fit))
        .flatMap((p) => chunks(p, MAX_SLIDE_LINES))
        .map((lines) => ({ lines: lines.map((l) => clean(l)) }));
  }
}

/** Seconds a loop waits on each slide, as the protocol accepts it. */
export function loopSeconds(s: number | undefined): number {
  const n = Math.round(Number(s));
  return Number.isFinite(n) ? Math.min(Math.max(n, 1), MAX_ADVANCE_SECONDS) : LOOP_SECONDS;
}

/** The run sheet to send to the daemon. */
export function build(draft: Draft, now = new Date()): RunSheet {
  return {
    id: draft.id,
    title: clean(draft.title, MAX_TITLE),
    items: draft.items.map((item): RunSheetItem => {
      const built = { id: item.id, kind: item.kind, title: clean(item.title, MAX_TITLE), slides: slidesFor(item, now) };
      return item.kind === "loop" ? { ...built, advanceSeconds: loopSeconds(item.seconds) } : built;
    }),
  };
}

/** Every image the run sheet shows, for sending them before it. */
export const imagesIn = (sheet: RunSheet): string[] =>
  sheet.items.flatMap((i) => i.slides.flatMap((s) => (s.image ? [s.image] : [])));
