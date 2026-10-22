import type { SyncStatus } from "@jivvy/protocol";

const changes = (n: number) => `${n} ${n === 1 ? "change" : "changes"}`;

/**
 * The one-line sync status for the tech lead, in plain words. `time` formats a moment (ms since
 * epoch) for the reader's locale. Everything is always saved on the church computer, so no
 * line ever sounds like something was lost.
 */
export function syncLine(s: SyncStatus, time: (ms: number) => string): string {
  const waiting = s.pending ? ` ${changes(s.pending)} waiting.` : "";
  switch (s.state) {
    case "no_plus":
      return "Cloud sync is off without Plus. Everything stays on this computer.";
    case "not_set_up":
      return "Cloud sync isn't linked to your church account yet. Everything is saved on this computer.";
    case "paused_for_service":
      return `Sync paused during the service.${waiting}`;
    case "offline":
      return `Offline: changes are saved here and sync when the internet is back.${waiting}`;
    default:
      if (s.pending) return `Syncing ${changes(s.pending)}…`;
      return s.lastSyncedAt === undefined ? "Synced." : `Synced at ${time(s.lastSyncedAt)}.`;
  }
}

/** The notice for a document changed here and elsewhere. */
export function noticeLine(n: SyncStatus["changedElsewhere"][number]): string {
  return n.copyId
    ? `"${n.title}" was changed here and elsewhere: both versions are kept.`
    : `"${n.title}" was changed here and elsewhere: the latest edit to each part is kept.`;
}
