/**
 * Themes: how lyrics and text look on the screens. One place defines the format (this file).
 *
 * A theme is a library document of kind "theme" (id + title like any document); its body is:
 *
 *   {
 *     "v": 1,                          format version; readers ignore a theme with a higher v
 *     "font": "sans",                  "sans" | "serif" | "mono"
 *     "bold": true,
 *     "size": 7,                       text height as % of the screen height (2 to 20); assumes 16:9 for now
 *     "text": "#FFFFFF",               #RRGGBB
 *     "shadow": true,                  dark shadow behind text, for busy backgrounds
 *     "background": { "kind": "color", "color": "#10224F" }
 *                 or { "kind": "image", "image": "<image id>", "color": "#10224F" }
 *   }
 *
 * "image" is an image id as the run sheet uses. Images are browser-local for now (kept in the
 * browser that added them, see media.ts), so anywhere else `color` is shown instead. A keyed
 * lyric layer (the stream's) ignores `background`: it draws only the text over the video.
 *
 * Which theme is in use is a "setting" document, id "active-theme": { "v": 1, "themeId": "..." }.
 * Add fields, never rename or remove, and keep reading v 1: the native lyric layer (apps/daemon/src/lyrics.rs) will read
 * these same bodies. The three font names are a fixed list; each renderer picks its own face.
 */
import type { Doc } from "@jivvy/protocol";

export const THEME_VERSION = 1;
export const FONTS = ["sans", "serif", "mono"] as const;
export type Font = (typeof FONTS)[number];
export type Background = { kind: "color"; color: string } | { kind: "image"; image: string; color: string };

export interface Theme {
  v: 1;
  font: Font;
  bold: boolean;
  size: number;
  text: string;
  shadow: boolean;
  background: Background;
}

export const MIN_SIZE = 2;
export const MAX_SIZE = 20;
const HEX = /^#[0-9A-Fa-f]{6}$/;

export const FONT_STACKS: Record<Font, string> = {
  sans: 'system-ui, "Segoe UI", Roboto, Helvetica, Arial, sans-serif',
  serif: 'Georgia, "Times New Roman", serif',
  mono: 'ui-monospace, Consolas, "Courier New", monospace',
};

/** Built in, always there; saved themes are added to these. Ids are fixed. */
export const BUILT_IN: { id: string; title: string; theme: Theme }[] = [
  {
    id: "jivvy-black",
    title: "Black",
    theme: { v: 1, font: "sans", bold: true, size: 7, text: "#FFFFFF", shadow: true, background: { kind: "color", color: "#000000" } },
  },
  {
    id: "jivvy-navy",
    title: "Navy",
    theme: { v: 1, font: "sans", bold: true, size: 7, text: "#F4F1EA", shadow: false, background: { kind: "color", color: "#10224F" } },
  },
  {
    id: "jivvy-paper",
    title: "Paper",
    theme: { v: 1, font: "serif", bold: false, size: 7, text: "#10224F", shadow: false, background: { kind: "color", color: "#F4F1EA" } },
  },
];
export const DEFAULT_THEME_ID = "jivvy-black";

const isObj = (x: unknown): x is Record<string, unknown> => typeof x === "object" && x !== null && !Array.isArray(x);
const isColor = (x: unknown): x is string => typeof x === "string" && HEX.test(x);

/** A saved theme's body as a theme; null if another version wrote it or it is malformed. */
export function themeFromDoc(doc: Doc): Theme | null {
  const b = doc.body;
  const bg = b.background;
  if (b.v !== THEME_VERSION || !isObj(bg) || !isColor(bg.color) || !isColor(b.text)) return null;
  if (typeof b.font !== "string" || !(FONTS as readonly string[]).includes(b.font)) return null;
  if (typeof b.size !== "number" || !(b.size >= MIN_SIZE && b.size <= MAX_SIZE)) return null;
  let background: Background;
  if (bg.kind === "color") background = { kind: "color", color: bg.color };
  else if (bg.kind === "image" && typeof bg.image === "string" && bg.image) background = { kind: "image", image: bg.image, color: bg.color };
  else return null;
  if (typeof b.bold !== "boolean" || typeof b.shadow !== "boolean") return null;
  return { v: 1, font: b.font as Font, bold: b.bold, size: b.size, text: b.text, shadow: b.shadow, background };
}

/** The theme as a document body, to save. */
export function themeBody(t: Theme): Record<string, unknown> {
  return { ...t, background: { ...t.background } };
}

/** An id for a theme saved from this browser. */
export function newThemeId(title: string): string {
  const base = title.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-|-$/g, "").slice(0, 40) || "theme";
  return `${base}-${crypto.randomUUID().slice(0, 8)}`;
}

/**
 * CSS custom properties for a screen. The text height is a share of the screen's height,
 * written in container-width units (a 16:9 screen is 9/16 as tall as it is wide).
 */
export function themeStyle(t: Theme): string {
  return [
    `--t-font:${FONT_STACKS[t.font]}`,
    `--t-weight:${t.bold ? 700 : 400}`,
    `--t-size:${(t.size * 9) / 16}cqw`,
    `--t-text:${t.text}`,
    `--t-shadow:${t.shadow ? "0 0 0.25em #000, 0 0 0.1em #000" : "none"}`,
    `--t-bg:${t.background.color}`,
  ].join(";");
}

export const ACTIVE_THEME_ID = "active-theme";

export function activeBody(themeId: string): Record<string, unknown> {
  return { v: 1, themeId };
}

/** The theme id a saved "active-theme" setting names; null if it isn't one this version reads. */
export function activeFromDoc(doc: Doc): string | null {
  const id = doc.body.themeId;
  return doc.body.v === 1 && typeof id === "string" && id ? id : null;
}
