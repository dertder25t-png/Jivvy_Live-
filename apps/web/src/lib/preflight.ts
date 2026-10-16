/**
 * The pre-flight screen's logic: the daemon's result as rows, each green, amber or red in plain
 * English with a fix. The upload test needs the internet; when it can't run, the last speed
 * measured is shown and nothing about the service is blocked.
 */
import type { Preflight, PreflightLevel, StateSnapshot } from "@jivvy/protocol";

export interface Row {
  id: string;
  level: PreflightLevel;
  label: string;
  message: string;
  fix?: string;
}

/** The daemon refuses pre-flight while streaming (the test would take the stream's bandwidth). */
export const streaming = (live: StateSnapshot | null): boolean =>
  live !== null && (live.stream !== "off" || live.streamWanted === true);

export const mbps = (kbps: number): string => `${(kbps / 1000).toFixed(1)} Mbps`;

const OFFLINE_FIX = "Check the internet connection, then run the check again. The service still runs without it: slides, screens and recording don't need the internet.";

function uploadRow(p: Preflight): Row {
  const label = "Internet upload";
  if (p.status === "failed" || p.uploadKbps === undefined)
    return { id: "upload", level: "warn", label, message: p.message || "Couldn't test the upload.", fix: OFFLINE_FIX };
  if (p.measured) return { id: "upload", level: "ok", label, message: `${mbps(p.uploadKbps)} up, measured just now.` };
  const when = p.measuredAt ? ` on ${new Date(p.measuredAt).toLocaleString()}` : "";
  return {
    id: "upload",
    level: "warn",
    label,
    message: `Couldn't test it just now. Last measured: ${mbps(p.uploadKbps)} up${when}.`,
    fix: OFFLINE_FIX,
  };
}

function streamingRow(p: Preflight): Row | null {
  if (p.status !== "done" || p.destinations.length === 0) return null;
  const fits = p.destinations.filter((d) => d.quality).length;
  const level: PreflightLevel = fits === p.destinations.length ? "ok" : fits === 0 ? "bad" : "warn";
  const fix = level === "ok" ? undefined : "Stream to fewer platforms, or lower their quality in the streaming settings.";
  return { id: "streaming", level, label: "Streaming", message: p.summary, ...(fix ? { fix } : {}) };
}

const ORDER: Record<PreflightLevel, number> = { bad: 0, warn: 1, ok: 2 };

/** Every row, red first, then amber, then green; in the daemon's order within each. */
export function rows(p: Preflight): Row[] {
  const all: Row[] = [uploadRow(p)];
  const s = streamingRow(p);
  if (s) all.push(s);
  for (const c of p.checks ?? []) {
    const level: PreflightLevel = c.level === "ok" || c.level === "bad" ? c.level : "warn"; // unknown: worth a look
    all.push({ id: c.id, level, label: c.label, message: c.message, ...(c.fix ? { fix: c.fix } : {}) });
  }
  return all.map((r, i) => ({ r, i })).sort((a, b) => ORDER[a.r.level] - ORDER[b.r.level] || a.i - b.i).map((x) => x.r);
}

/** The worst level among the rows. */
export const overall = (list: Row[]): PreflightLevel =>
  list.some((r) => r.level === "bad") ? "bad" : list.some((r) => r.level === "warn") ? "warn" : "ok";

export const headline = (list: Row[]): string => {
  const red = list.filter((r) => r.level === "bad").length;
  const amber = list.filter((r) => r.level === "warn").length;
  if (red) return `${red} to fix before the service${amber ? `, ${amber} worth a look` : ""}.`;
  if (amber) return `Ready. ${amber} worth a look.`;
  return "Ready for the service.";
};
