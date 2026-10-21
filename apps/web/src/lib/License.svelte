<script lang="ts">
  import type { Ack, Command, LicenseStatus } from "@jivvy/protocol";

  /**
   * The license panel for the tech lead: the trial or license on the church computer, and a box
   * to paste a key from live.jivvy.org. The church computer checks the key offline; nothing here
   * ever stops a service. Not shown in volunteer mode.
   */
  let { ready, send }: { ready: boolean; send: (command: Command) => Promise<Ack> | undefined } = $props();

  let open = $state(false);
  let status = $state<LicenseStatus | null>(null);
  let pasted = $state("");
  let note = $state("");
  let busy = $state(false);

  const day = (ms: number) => new Date(ms).toLocaleDateString(undefined, { year: "numeric", month: "long", day: "numeric" });

  async function load() {
    const ack = await send({ type: "license.get" });
    if (ack?.ok && ack.license) status = ack.license;
  }

  $effect(() => {
    if (open && ready) void load();
  });

  async function save() {
    const key = pasted.trim();
    if (!key || busy) return;
    busy = true;
    note = "";
    try {
      const ack = await send({ type: "license.set", key });
      if (!ack) note = "Not connected to the church computer.";
      else if (!ack.ok) note = ack.message;
      else {
        if (ack.license) status = ack.license;
        pasted = "";
        note = "License saved.";
      }
    } finally {
      busy = false;
    }
  }
</script>

<details class="license" bind:open>
  <summary>License</summary>
  <div class="panel">
    {#if !ready}
      <p>Connect to the church computer to see its license.</p>
    {:else if status}
      {#if status.state === "licensed" && status.license}
        <p>
          Licensed ({status.license.plan}). Plus {status.plusActive ? "until" : "ended"} {day(status.license.plusUntil)}; new
          features {status.updatesActive ? "until" : "ended"} {day(status.license.updatesUntil)}.
        </p>
      {:else if status.state === "trial"}
        <p>Free trial until {day(status.trialEndsAt)}.</p>
      {:else}
        <p>Free trial ended {day(status.trialEndsAt)}. Services keep running as normal.</p>
      {/if}
      {#if status.notice}<p class="notice" role="status">{status.notice}</p>{/if}
      {#if status.key}
        <label for="license-current">Key on this computer</label>
        <input id="license-current" readonly value={status.key} spellcheck="false" />
      {/if}
    {/if}
    {#if ready}
      <label for="license-key">Paste a license key from live.jivvy.org</label>
      <textarea id="license-key" rows="3" bind:value={pasted} spellcheck="false" autocomplete="off" placeholder="JL1."></textarea>
      <button onclick={save} disabled={busy || !pasted.trim()}>{busy ? "Saving" : "Save key"}</button>
      {#if note}<span class="note" role="status">{note}</span>{/if}
    {/if}
  </div>
</details>

<style>
  .license {
    position: relative;
  }
  .panel {
    position: absolute;
    right: 0;
    z-index: 10;
    width: min(24rem, 90vw);
    padding: 0.8rem;
    background: #f4f1ea;
    color: #10224f;
    border: 1px solid #10224f;
    border-radius: 6px;
    display: grid;
    gap: 0.4rem;
  }
  .panel p {
    margin: 0;
  }
  .notice {
    font-size: 0.9rem;
  }
  label {
    font-size: 0.8rem;
  }
  input,
  textarea {
    font-family: monospace;
    font-size: 0.75rem;
    width: 100%;
    box-sizing: border-box;
  }
  .note {
    font-size: 0.8rem;
  }
</style>
