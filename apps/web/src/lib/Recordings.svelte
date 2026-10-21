<script lang="ts">
  import type { Ack, Command, RecordingSet } from "@jivvy/protocol";
  import { size, title } from "./recordings";

  /**
   * Recordings made on the church computer, newest first, each labelled by the plan that was
   * live when it started. Read-only: no deleting, no downloads. The files are opened on the
   * church computer itself.
   */
  let { ready, send }: { ready: boolean; send: (command: Command) => Promise<Ack> | undefined } = $props();

  let sets = $state<RecordingSet[] | null>(null);
  let error = $state("");
  let folder = $state("");
  let loading = $state(false);

  async function refresh() {
    if (!ready) return;
    loading = true;
    error = "";
    const ack = await send({ type: "recording.list" });
    loading = false;
    if (!ack) error = "Not connected.";
    else if (!ack.ok) error = ack.code === "unknown_command" ? "This church computer is too old to list recordings." : ack.message;
    else {
      sets = ack.recordings ?? [];
      folder = ack.recordingsFolder ?? "";
    }
  }
  // Loads when opened and again if the connection comes back.
  $effect(() => {
    if (ready) void refresh();
  });
</script>

<section class="recordings" aria-label="Recordings">
  <div class="head">
    <div>
      <h2>Recordings</h2>
      <p class="hint">
        Open the recordings folder on the church computer to watch or copy them{#if folder}: <code>{folder}</code>{/if}.
        Each MP4 appears there a little while after the recording stops.
      </p>
    </div>
    <button onclick={refresh} disabled={!ready || loading}>{loading ? "Loading…" : "Refresh"}</button>
  </div>
  {#if !ready}<p class="note">Connect to the church computer to see its recordings.</p>{/if}
  {#if error}<p class="note" role="alert">{error}</p>{/if}
  {#if sets && !sets.length}<p class="note">No recordings yet.</p>{/if}
  {#if sets?.length}
    <ul>
      {#each sets as r (r.folder)}
        <li>
          <span class="name">{title(r)}</span>
          <span class="size">{size(r.bytes)}</span>
        </li>
      {/each}
    </ul>
  {/if}
</section>

<style>
  .recordings {
    max-width: 1100px;
    margin: 16px auto 32px;
    padding: 0 20px;
  }
  .head {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: space-between;
    gap: 8px 16px;
  }
  h2 {
    font-size: 18px;
    margin: 0;
    color: var(--navy);
  }
  .hint {
    color: var(--muted);
    font-size: 13px;
    margin: 4px 0;
  }
  .note {
    font-size: 14px;
    margin: 10px 0 0;
  }
  ul {
    list-style: none;
    margin: 12px 0 0;
    padding: 0;
    display: grid;
    gap: 8px;
  }
  li {
    display: flex;
    justify-content: space-between;
    gap: 12px;
    padding: 10px 12px;
    background: #fff;
    border: 1px solid rgba(16, 34, 79, 0.15);
    border-radius: 8px;
  }
  .size {
    color: var(--muted);
    white-space: nowrap;
  }
</style>
