/** Wording for the recordings list: local time, length, size and whether the MP4 is ready. */
import type { RecordingSet } from "@jivvy/protocol";

export function duration(secs: number | undefined): string {
  if (secs === undefined) return "";
  const mins = Math.round(secs / 60);
  if (mins < 1) return "under a minute";
  if (mins < 60) return `${mins} min`;
  return `${Math.floor(mins / 60)} h ${String(mins % 60).padStart(2, "0")} min`.replace(" 00 min", "");
}

export function size(bytes: number): string {
  const gb = bytes / 1024 ** 3;
  if (gb >= 1) return `${gb.toFixed(1)} GB`;
  return `${Math.max(1, Math.round(bytes / 1024 ** 2))} MB`;
}

export function mp4Words(mp4: RecordingSet["mp4"]): string {
  switch (mp4) {
    case "ready":
      return "MP4 ready";
    case "recording":
      return "Recording now";
    case "converting":
      return "Converting to MP4…";
    case "waiting":
      return "Waiting to convert to MP4";
    default:
      return "No video";
  }
}

/** The start in this computer's time zone (or `timeZone`, for tests). */
export function startedAt(ms: number, withTime: boolean, timeZone?: string): string {
  const opts: Intl.DateTimeFormatOptions = { month: "short", day: "numeric", year: "numeric", timeZone };
  if (withTime) Object.assign(opts, { hour: "numeric", minute: "2-digit" });
  return new Date(ms).toLocaleString("en-US", opts);
}

/** "Sunday 9:30 · Oct 19, 2026 · 1 h 12 min · MP4 ready"; a set without a plan shows its time instead. */
export function title(r: RecordingSet, timeZone?: string): string {
  const parts = [
    r.label ?? "Recording",
    startedAt(r.startedMs, !r.label, timeZone),
    duration(r.durationSecs),
    mp4Words(r.mp4),
  ];
  return parts.filter(Boolean).join(" · ");
}
