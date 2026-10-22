import type { StateSnapshot } from "@jivvy/protocol";

/**
 * Whether a service may be on, from what every screen sees: streaming or recording wanted, or
 * a run sheet on the screens. Errs towards yes (the church computer's own rule also lets a run
 * sheet left up for 3 hours count as over), so a license notice never shows during a service.
 */
export function maybeInService(live: StateSnapshot | null): boolean {
  if (!live) return false;
  return Boolean(live.streamWanted || live.recordingWanted || (live.runSheetId !== undefined && !live.black));
}
