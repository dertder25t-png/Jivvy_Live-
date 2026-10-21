<script lang="ts">
  import type { Ack, Command, LicenseStatus, StateSnapshot } from "@jivvy/protocol";
  import { maybeInService } from "./license";

  /**
   * The license panel for the tech lead: the trial or license on the church computer, and a box
   * to paste a key from live.jivvy.org. The church computer checks the key offline; nothing here
   * ever stops a service. License commands are the church computer's alone, so a paired device
   * is told where to do it instead. Not shown in volunteer mode.
   */
  let {
    ready,
    live,
    send,
  }: { ready: boolean; live: StateSnapshot | null; send: (command: Command) => Promise<Ack> | undefined } = $props();

  let open = $state(false);
  let status = $state<LicenseStatus | null>(null);
  let churchOnly = $state(false);
  let pasted = $state("");
  let note = $state("");
  let busy = $state(false);

  // The notice goes the moment a service may have started, and comes back (if the church
  // computer still has one) once it's over: the effect below asks again on every change.
  const inService = $derived(maybeInService(live));

  const day = (ms: number) => new Date(ms).toLocaleDateString(undefined, { year: "numeric", month: "long", day: "numeric" });

  async function load() {
    const ack = await send({ type: "license.get" });
    churchOnly = ack?.ok === false && ack.code === "unauthorized";
    if (ack?.ok && ack.license) status = ack.license;
  }

  $effect(() => {
    void inService;
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
    {:else if churchOnly}
      <p>
        The license is set on the church computer itself. There, run <code>jivvy-ctl license</code> to see it, or
        <code>jivvy-ctl license set</code> followed by the key from live.jivvy.org.
      </p>
    {:else}
      {#if status}
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
        {#if status.notice && !inService}<p class="notice" role="status">{status.notice}</p>{/if}
        {#if status.keyHint}<p class="hint">Key on this computer: <code>{status.keyHint}</code></p>{/if}
      {/if}
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
  label,
  .hint,
  .note {
    font-size: 0.8rem;
  }
  textarea {
    font-family: monospace;
    font-size: 0.75rem;
    width: 100%;
    box-sizing: border-box;
  }
</style>
