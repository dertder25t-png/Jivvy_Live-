<script lang="ts">
  import { exportFiles, readAll, zip } from "./export";
  import type { Sender } from "./library";

  let { ready, sender }: { ready: boolean; sender: () => Sender | null } = $props();
  let busy = $state(false);
  let note = $state("");

  async function run() {
    const conn = sender();
    if (!conn || busy) return;
    busy = true;
    note = "";
    try {
      const { docs, skipped } = await readAll(conn);
      const files = exportFiles(docs);
      const blob = new Blob([zip(files) as BlobPart], { type: "application/zip" });
      const a = document.createElement("a");
      a.href = URL.createObjectURL(blob);
      a.download = `jivvy-live-export-${new Date().toISOString().slice(0, 10)}.zip`;
      a.click();
      setTimeout(() => URL.revokeObjectURL(a.href), 10_000);
      note = `Exported ${files.length - 1} files.` + (skipped ? ` ${skipped} could not be read and were left out.` : "");
    } catch (e) {
      note = e instanceof Error ? e.message : "Export failed.";
    } finally {
      busy = false;
    }
  }
</script>

<button class="export" onclick={run} disabled={!ready || busy} title="Songs, run sheets, themes and settings as plain files">
  {busy ? "Exporting…" : "Export"}
</button>
{#if note}<span class="note" role="status">{note}</span>{/if}

<style>
  .note {
    font-size: 0.8rem;
    margin-left: 0.4rem;
  }
</style>
