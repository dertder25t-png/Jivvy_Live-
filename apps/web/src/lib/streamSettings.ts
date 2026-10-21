import type { StreamConfigDestination, StreamConfigInput, StreamConfigView } from "@jivvy/protocol";

/** Where a new destination starts from. The address is the server only; the key is typed in. */
export const PRESETS = [
  { id: "youtube", label: "YouTube (RTMPS)", name: "YouTube", url: "rtmps://a.rtmps.youtube.com:443/live2" },
  { id: "youtube-hls", label: "YouTube (HLS upload)", name: "YouTube", url: "https://a.upload.youtube.com/http_upload_hls" },
  { id: "facebook", label: "Facebook", name: "Facebook", url: "rtmps://live-api-s.facebook.com:443/rtmp/" },
  { id: "custom", label: "Custom RTMP", name: "", url: "" },
] as const;

/** One destination being edited. `key` is only what was typed this visit: saved keys never come back. */
export interface Row {
  id: string;
  name: string;
  url: string;
  key: string;
  hasKey: boolean;
  enabled: boolean;
  maxQuality: string;
  whenShort: string;
}

export interface Draft {
  rows: Row[];
  advanced: boolean;
}

export const fromView = (view: StreamConfigView): Draft => ({
  rows: view.destinations.map((d: StreamConfigDestination) => ({
    id: d.id,
    name: d.name,
    url: d.url,
    key: "",
    hasKey: d.hasKey,
    enabled: d.enabled,
    maxQuality: d.maxQuality ?? "",
    whenShort: d.whenShort ?? "",
  })),
  advanced: view.bandwidth?.mode === "advanced",
});

/** A fresh id for a destination: the preset's name, made unique among the ones already there. */
export function newRowId(base: string, taken: string[]): string {
  const slug = base.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-|-$/g, "") || "stream";
  let id = slug;
  for (let n = 2; taken.includes(id); n++) id = `${slug}-${n}`;
  return id;
}

export function addRow(draft: Draft, preset: (typeof PRESETS)[number]): Draft {
  const id = newRowId(preset.name || "custom", draft.rows.map((r) => r.id));
  const row: Row = { id, name: preset.name, url: preset.url, key: "", hasKey: false, enabled: true, maxQuality: "", whenShort: "" };
  return { ...draft, rows: [...draft.rows, row] };
}

/** Moves a row up (-1) or down (+1); the first one is the main platform. */
export function moveRow(draft: Draft, index: number, by: -1 | 1): Draft {
  const to = index + by;
  if (to < 0 || to >= draft.rows.length) return draft;
  const rows = [...draft.rows];
  [rows[index], rows[to]] = [rows[to]!, rows[index]!];
  return { ...draft, rows };
}

/** The `stream.config.set` payload. A key is sent only if one was typed; the quality limits only in Advanced. */
export function toInput(draft: Draft): StreamConfigInput {
  return {
    destinations: draft.rows.map((r) => ({
      id: r.id,
      name: r.name.trim(),
      url: r.url.trim(),
      ...(r.key.trim() ? { key: r.key.trim() } : {}),
      enabled: r.enabled,
      ...(draft.advanced && r.maxQuality ? { maxQuality: r.maxQuality } : {}),
      ...(draft.advanced && r.whenShort ? { whenShort: r.whenShort } : {}),
    })),
    bandwidth: { mode: draft.advanced ? "advanced" : "auto" },
  };
}
