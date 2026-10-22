<script lang="ts">
  import type { Ack, Command, SyncStatus } from "@jivvy/protocol";
  import { onDestroy } from "svelte";
  import { noticeLine, syncLine } from "./sync";

  /**
   * One line on how the library's cloud sync is going (Plus), with any "changed elsewhere"
   * notices. The church computer syncs by itself; this only asks it now and then. An older
   * church computer without sync shows nothing.
   */
  let { ready, send }: { ready: boolean; send: (command: Command) => Promise<Ack> | undefined } = $props();

  let status = $state<SyncStatus | null>(null);
  const time = (ms: number) => new Date(ms).toLocaleTimeString(undefined, { hour: "numeric", minute: "2-digit" });

  async function load() {
    const ack = await send({ type: "sync.status" });
    status = ack?.ok && ack.sync ? ack.sync : null;
  }

  let timer: ReturnType<typeof setInterval> | undefined;
  $effect(() => {
    clearInterval(timer);
    if (!ready) return void (status = null);
    void load();
    timer = setInterval(load, 15_000);
  });
  onDestroy(() => clearInterval(timer));
</script>

{#if status}
  <div class="sync" role="status">
    <span>{syncLine(status, time)}</span>
    {#each status.changedElsewhere.slice(0, 3) as n (n.kind + n.id + n.at)}
      <span class="notice">{noticeLine(n)}</span>
    {/each}
  </div>
{/if}

<style>
  .sync {
    display: grid;
    gap: 0.2rem;
    font-size: 0.8rem;
    color: #10224f;
  }
  .notice {
    border-left: 3px solid #2e6bff;
    padding-left: 0.4rem;
  }
</style>
