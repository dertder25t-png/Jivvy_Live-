<script lang="ts">
  import type { Ack, Command, Preflight, StateSnapshot } from "@jivvy/protocol";
  import { headline, mbps, overall, rows, streaming } from "./preflight";

  /**
   * The pre-flight check: tests the internet upload and reads the church computer's own checks
   * (disk, streaming settings, video check, screens, camera, microphone), then lists each one
   * green, amber or red with a fix, and what each platform will get. It only reads and measures,
   * so a volunteer may run it too. Not while streaming: the church computer refuses then.
   */
  let {
    live,
    ready,
    result,
    send,
  }: {
    live: StateSnapshot | null;
    ready: boolean;
    /** The latest result, from whichever device asked (every screen hears it). */
    result: Preflight | null;
    send: (command: Command) => Promise<Ack> | undefined;
  } = $props();

  let running = $state(false);
  let error = $state("");
  // The result event ends the wait (the ack only says it started).
  $effect(() => {
    if (result && result.status !== "measuring") running = false;
  });

  const isStreaming = $derived(streaming(live));
  const list = $derived(result && result.status !== "measuring" ? rows(result) : []);
  const level = $derived(overall(list));

  async function run() {
    error = "";
    running = true;
    const ack = await send({ type: "stream.preflight" });
    if (!ack || !ack.ok) {
      running = false;
      error = ack?.message ?? "Not connected.";
    }
  }
</script>

<section class="preflight" aria-label="Pre-flight check">
  <div class="head">
    <div>
      <h2>Pre-flight check</h2>
      <p class="hint">Tests the internet upload and the church computer in under a minute. Run it before the service.</p>
    </div>
    <button class="primary" onclick={run} disabled={!ready || running || isStreaming}>
      {running ? "Checking…" : result ? "Check again" : "Run pre-flight"}
    </button>
  </div>

  {#if isStreaming}
    <p class="note">Pre-flight doesn't run while streaming: the upload test would take the stream's bandwidth.</p>
  {:else if !ready}
    <p class="note">Connect to the church computer to run it.</p>
  {/if}
  {#if error}<p class="note" role="alert">{error}</p>{/if}
  {#if running}<p class="hint" role="status">Measuring the internet upload…</p>{/if}

  {#if list.length && result}
    <div class="summary" data-level={level}>
      <strong>{headline(list)}</strong>
      {#if result.summary && result.destinations.length}
        <span class="stream">{result.summary}{#if result.uploadKbps} · {mbps(result.uploadKbps)} up{/if}</span>
      {/if}
    </div>
    <ul class="checks">
      {#each list as r (r.id)}
        <li data-level={r.level}>
          <span class="dot" aria-hidden="true"></span>
          <div>
            <span class="label">{r.label}</span>
            <span class="sr">{r.level === "ok" ? "OK" : r.level === "warn" ? "Worth a look" : "Fix before the service"}:</span>
            {r.message}
            {#if r.fix}<div class="fix">{r.fix}</div>{/if}
          </div>
        </li>
      {/each}
    </ul>
  {/if}
</section>

<style>
  .preflight {
    --ok: #2e9d5b;
    --amber: #d99a00;
    --bad: #c0392b;
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
    padding: 8px 12px;
    border-radius: 6px;
    background: var(--card);
    border: 1px solid var(--line);
  }
  button {
    border: 1px solid var(--line);
    background: var(--card);
    border-radius: 6px;
    padding: 10px 18px;
    cursor: pointer;
  }
  button:disabled {
    opacity: 0.45;
    cursor: default;
  }
  button.primary {
    background: var(--blue);
    border-color: var(--blue);
    color: #fff;
    font-weight: 600;
  }
  .summary {
    display: flex;
    flex-wrap: wrap;
    gap: 4px 14px;
    align-items: baseline;
    margin-top: 14px;
    padding: 12px 16px;
    border-radius: 8px;
    background: var(--card);
    border: 1px solid var(--line);
    border-left: 4px solid var(--ok);
  }
  .summary[data-level="warn"] {
    border-left-color: var(--amber);
  }
  .summary[data-level="bad"] {
    border-left-color: var(--bad);
  }
  .stream {
    color: var(--muted);
    font-size: 14px;
  }
  .checks {
    list-style: none;
    padding: 0;
    margin: 10px 0 0;
  }
  .checks li {
    display: flex;
    gap: 10px;
    align-items: baseline;
    padding: 10px 12px;
    border-bottom: 1px solid var(--line);
    font-size: 14px;
  }
  .dot {
    flex: none;
    width: 10px;
    height: 10px;
    border-radius: 50%;
    background: var(--ok);
    transform: translateY(1px);
  }
  li[data-level="warn"] .dot {
    background: var(--amber);
  }
  li[data-level="bad"] .dot {
    background: var(--bad);
  }
  .label {
    font-weight: 600;
    margin-right: 6px;
  }
  .fix {
    color: var(--muted);
    font-size: 13px;
    margin-top: 2px;
  }
  li[data-level="bad"] .fix {
    color: var(--navy);
    font-weight: 600;
  }
  .sr {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip-path: inset(50%);
  }
</style>
