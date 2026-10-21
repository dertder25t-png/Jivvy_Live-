<script lang="ts">
  import type { Ack, Command, StateSnapshot } from "@jivvy/protocol";
  import { streaming } from "./preflight";
  import { PRESETS, addRow, fromView, moveRow, toInput, type Draft } from "./streamSettings";

  /**
   * Streaming settings: the destinations (YouTube, Facebook, any RTMP server) and the advanced
   * bandwidth settings that used to mean editing stream.json on the church computer. Stream keys
   * are write-only: type one to set or replace it; a saved key is never shown, only "Key saved".
   * Not available while streaming (a save would reconnect the platforms), and not in volunteer mode.
   */
  let {
    live,
    ready,
    send,
  }: {
    live: StateSnapshot | null;
    ready: boolean;
    send: (command: Command) => Promise<Ack> | undefined;
  } = $props();

  let draft = $state<Draft>({ rows: [], advanced: false });
  let loaded = $state(false);
  let busy = $state(false);
  let message = $state("");
  let failed = $state(false);
  let preset = $state<string>(PRESETS[0].id);

  const isStreaming = $derived(streaming(live));

  async function load() {
    const ack = await send({ type: "stream.config.get" });
    if (ack?.ok && ack.streamConfig) {
      draft = fromView(ack.streamConfig);
      loaded = true;
      message = "";
    } else {
      failed = true;
      message = ack && !ack.ok ? ack.message : "Not connected.";
    }
  }

  // Load when the church computer is reachable; start again after a reconnect.
  $effect(() => {
    if (ready && !loaded) void load();
    if (!ready) loaded = false;
  });

  async function save() {
    busy = true;
    failed = false;
    message = "";
    const ack = await send({ type: "stream.config.set", config: toInput(draft) });
    busy = false;
    if (ack?.ok && ack.streamConfig) {
      draft = fromView(ack.streamConfig); // typed keys are gone from the screen once saved
      message = "Saved. The next stream uses these settings.";
    } else {
      failed = true;
      message = ack && !ack.ok ? ack.message : "Not connected. Nothing was saved.";
    }
  }

  const add = () => {
    const p = PRESETS.find((x) => x.id === preset) ?? PRESETS[0];
    draft = addRow(draft, p);
  };
  const remove = (i: number) => {
    const name = draft.rows[i]?.name || "this destination";
    if (confirm(`Remove ${name}? Its saved stream key is deleted when you save.`)) draft.rows.splice(i, 1);
  };
</script>

<section class="stream-settings" aria-label="Streaming settings">
  <h2>Streaming settings</h2>
  <p class="hint">
    Where the stream goes. The first destination is the main one. Stream keys are never shown again once saved.
  </p>

  {#if isStreaming}
    <p class="note">Stop the stream to change these settings. Saving during a stream would reconnect it and could drop it.</p>
  {:else if !ready}
    <p class="note">Connect to the church computer to change these.</p>
  {/if}

  {#each draft.rows as row, i (row.id)}
    <fieldset class="dest" disabled={!ready || isStreaming}>
      <legend>{row.name || "Destination"}{#if i === 0} (main){/if}</legend>
      <label>Name <input bind:value={row.name} maxlength="64" /></label>
      <label>Server address <input bind:value={row.url} spellcheck="false" autocomplete="off" /></label>
      <label>
        Stream key
        <input
          type="password"
          bind:value={row.key}
          autocomplete="off"
          spellcheck="false"
          placeholder={row.hasKey ? "Key saved. Type to replace it." : "Paste the stream key"}
        />
      </label>
      {#if row.hasKey && !row.key}<span class="saved">Key saved</span>{/if}
      <label class="inline"><input type="checkbox" bind:checked={row.enabled} /> Use this destination</label>
      {#if draft.advanced}
        <label>
          Never above
          <select bind:value={row.maxQuality}>
            <option value="">No limit</option>
            <option value="1080p">1080p</option>
            <option value="720p">720p</option>
            <option value="480p">480p</option>
            <option value="360p">360p</option>
          </select>
        </label>
        <label>
          When the upload runs short
          <select bind:value={row.whenShort}>
            <option value="">Lower the quality</option>
            <option value="audioOnly">Send audio only</option>
            <option value="pause">Pause this destination</option>
          </select>
        </label>
      {/if}
      <div class="actions">
        <button type="button" onclick={() => (draft = moveRow(draft, i, -1))} disabled={i === 0} aria-label="Move up">Up</button>
        <button type="button" onclick={() => (draft = moveRow(draft, i, 1))} disabled={i === draft.rows.length - 1} aria-label="Move down">Down</button>
        <button type="button" class="quiet" onclick={() => remove(i)}>Remove</button>
      </div>
    </fieldset>
  {:else}
    {#if loaded}<p class="hint">No destinations yet. Add one below.</p>{/if}
  {/each}

  <div class="add">
    <select bind:value={preset} aria-label="Kind of destination" disabled={!ready || isStreaming}>
      {#each PRESETS as p (p.id)}<option value={p.id}>{p.label}</option>{/each}
    </select>
    <button type="button" onclick={add} disabled={!ready || isStreaming || !loaded || draft.rows.length >= 16}>Add destination</button>
  </div>

  <label class="inline">
    <input type="checkbox" bind:checked={draft.advanced} disabled={!ready || isStreaming} />
    Advanced bandwidth settings (a quality limit and a fallback for each destination)
  </label>

  <div class="save">
    <button class="primary" type="button" onclick={save} disabled={!ready || isStreaming || busy || !loaded}>
      {busy ? "Saving…" : "Save"}
    </button>
    {#if message}<span class="msg" role={failed ? "alert" : "status"} data-failed={failed}>{message}</span>{/if}
  </div>
</section>

<style>
  .stream-settings {
    max-width: 640px;
    margin: 16px auto;
    padding: 0 16px;
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
    margin: 10px 0;
    padding: 8px 12px;
    border-radius: 6px;
    background: var(--card);
    border: 1px solid var(--line);
  }
  .dest {
    border: 1px solid var(--line);
    border-radius: 8px;
    margin: 12px 0;
    padding: 10px 14px;
    display: grid;
    gap: 8px;
    background: var(--card);
  }
  legend {
    font-weight: 600;
    color: var(--navy);
    padding: 0 6px;
  }
  label {
    display: grid;
    gap: 2px;
    font-size: 13px;
  }
  label.inline {
    display: flex;
    gap: 8px;
    align-items: center;
    margin: 8px 0;
  }
  input:not([type="checkbox"]),
  select {
    padding: 8px 10px;
    border: 1px solid var(--line);
    border-radius: 6px;
    font-size: 14px;
  }
  .saved {
    font-size: 12px;
    color: var(--muted);
  }
  .actions,
  .add,
  .save {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
    align-items: center;
  }
  .add,
  .save {
    margin: 12px 0;
  }
  button {
    border: 1px solid var(--line);
    background: var(--card);
    border-radius: 6px;
    padding: 8px 16px;
    cursor: pointer;
  }
  button:disabled {
    opacity: 0.45;
    cursor: default;
  }
  .msg[data-failed="true"] {
    color: var(--navy);
    font-weight: 600;
  }
</style>
