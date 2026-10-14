<script lang="ts">
  import { runSheetSlides, type RunSheet, type StateSnapshot } from "@jivvy/protocol";
  import { onDestroy, onMount } from "svelte";
  import { DaemonConnection, type Status } from "$lib/daemon";
  import { addImage, upload } from "$lib/media";
  import { build, imagesIn, newId, newItem, type Draft, type DraftItem, type ItemKind } from "$lib/runsheet";
  import Thumb from "$lib/Thumb.svelte";

  const DRAFT_KEY = "jivvy.draft";
  const ADDRESS_KEY = "jivvy.address";
  const KINDS: { kind: ItemKind; label: string }[] = [
    { kind: "song", label: "Song" },
    { kind: "scripture", label: "Scripture" },
    { kind: "countdown", label: "Countdown" },
    { kind: "loop", label: "Pre-service loop" },
    { kind: "image", label: "Images" },
    { kind: "text", label: "Text" },
  ];

  // Per-browser conveniences only: the run sheet that matters is the one on the daemon.
  function stored<T>(key: string, fallback: T): T {
    try {
      const raw = localStorage.getItem(key);
      return raw ? (JSON.parse(raw) as T) : fallback;
    } catch {
      return fallback;
    }
  }
  function store(key: string, value: unknown): void {
    try {
      localStorage.setItem(key, JSON.stringify(value));
    } catch {
      // storage full or blocked: the draft lives for this visit
    }
  }

  let draft = $state<Draft>(stored(DRAFT_KEY, { id: newId(), title: "Sunday service", items: [] }));
  let address = $state(stored(ADDRESS_KEY, "ws://127.0.0.1:47801"));
  let deviceName = $state("Run sheet (browser)");
  let status = $state<Status>({ kind: "offline", message: "Not connected." });
  let live = $state<StateSnapshot | null>(null);
  let onScreens = $state<RunSheet | null>(null);
  let sendError = $state("");
  let sending = $state("");
  let conn: DaemonConnection | null = null;

  $effect(() => store(DRAFT_KEY, draft));

  const built = $derived(build(draft));
  const builtJson = $derived(JSON.stringify(built));
  // Compared with what the daemon has, so it stays true after a reload or another device's send.
  const unsent = $derived(builtJson !== JSON.stringify(onScreens));
  const slides = $derived(onScreens ? runSheetSlides(onScreens) : []);
  const current = $derived(live?.slideIndex ?? 0);
  const ready = $derived(status.kind === "ready");

  function connect() {
    conn?.close();
    store(ADDRESS_KEY, address);
    live = null;
    onScreens = null;
    conn = new DaemonConnection(address.trim(), {
      status: (s) => (status = s),
      state: (s) => (live = s),
      runSheet: (r) => (onScreens = r),
    });
    conn.connect();
  }
  // Back on the church computer used last time, straight away.
  onMount(() => {
    if (stored<string | null>(ADDRESS_KEY, null)) connect();
  });
  onDestroy(() => conn?.close());

  async function send() {
    if (!conn) return;
    sendError = "";
    // Built now, not the copy kept for the editor: a countdown whose time has passed while
    // the page was open counts down to the next one, not to a moment already gone.
    const sheet = build(draft);
    // Images first: the church computer refuses a run sheet showing one it doesn't have.
    const failed = await upload(conn, imagesIn(sheet), (done, total) => {
      sending = total ? `Sending images ${done} of ${total}…` : "";
    });
    sending = "";
    if (failed) return void (sendError = failed);
    const ack = await conn.send({ type: "runsheet.set", runSheet: sheet });
    if (!ack.ok) sendError = ack.message;
  }

  let adding = $state("");
  async function addImages(item: DraftItem, files: FileList | null) {
    sendError = "";
    for (const [n, file] of [...(files ?? [])].entries()) {
      adding = `Adding ${n + 1} of ${files?.length}…`;
      try {
        item.images = [...(item.images ?? []), await addImage(file)];
      } catch (e) {
        sendError = e instanceof Error ? e.message : String(e);
      }
    }
    adding = "";
  }

  const go = (type: "slide.next" | "slide.prev") => conn?.send({ type });
  const goto = (index: number) => conn?.send({ type: "slide.goto", index });
  const black = () => conn?.send({ type: "output.black", on: !live?.black });

  function add(kind: ItemKind) {
    draft.items.push(newItem(kind));
  }
  function move(i: number, by: number) {
    const [item] = draft.items.splice(i, 1);
    if (item) draft.items.splice(i + by, 0, item);
  }
  function remove(i: number) {
    draft.items.splice(i, 1);
  }
  function startNew() {
    if (draft.items.length && !confirm("Start a new run sheet? This one stays on the screens until you send the new one."))
      return;
    draft = { id: newId(), title: "Sunday service", items: [] };
  }

  /** Arrow keys, Page Up/Down and Space move the slides, except while typing. */
  function keydown(e: KeyboardEvent) {
    const t = e.target as HTMLElement;
    if (!ready || t.closest("input, textarea, select, [contenteditable]")) return;
    if (["ArrowRight", "ArrowDown", "PageDown", " "].includes(e.key)) go("slide.next");
    else if (["ArrowLeft", "ArrowUp", "PageUp"].includes(e.key)) go("slide.prev");
    else if (e.key === "b" || e.key === "B") black();
    else return;
    e.preventDefault();
  }

  const kindLabel = (k: string) => KINDS.find((x) => x.kind === k)?.label ?? "Text";
</script>

<svelte:window onkeydown={keydown} />

<header>
  <img class="brand" src="/brand/jivvy-live-lockup.svg" alt="Jivvy Live" width="147" height="60" />
  <form
    class="connect"
    onsubmit={(e) => {
      e.preventDefault();
      connect();
    }}
  >
    <label for="address">Church computer</label>
    <input id="address" bind:value={address} spellcheck="false" autocomplete="off" />
    <button type="submit">{ready ? "Reconnect" : "Connect"}</button>
  </form>
  <div class="status" data-kind={status.kind} role="status">
    {#if status.kind === "ready"}Connected
    {:else if status.kind === "connecting"}Connecting…
    {:else if status.kind === "pairing"}Waiting for approval
    {:else if status.kind === "unpaired"}Not paired
    {:else}{status.message}{/if}
  </div>
</header>

{#if status.kind === "unpaired"}
  <section class="banner">
    <p>
      This browser isn't paired with the church computer yet. Open pairing there, then ask to pair.
      {#if status.message}<br /><strong>{status.message}</strong>{/if}
    </p>
    <form
      onsubmit={(e) => {
        e.preventDefault();
        conn?.pair(deviceName);
      }}
    >
      <label>Name this device <input bind:value={deviceName} maxlength="64" required /></label>
      <button type="submit">Ask to pair</button>
    </form>
  </section>
{:else if status.kind === "pairing"}
  <section class="banner">
    <p>Approve this device on the church computer if it shows the same code:</p>
    <p class="code">{status.code}</p>
    <p class="hint">Request <code>{status.requestId}</code></p>
  </section>
{/if}

<main>
  <section class="plan" aria-label="Run sheet">
    <div class="plan-head">
      <input class="sheet-title" bind:value={draft.title} aria-label="Run sheet title" maxlength="200" />
      <button class="quiet" onclick={startNew}>New</button>
    </div>

    {#each draft.items as item, i (item.id)}
      <article class="item">
        <div class="item-head">
          <span class="kind">{kindLabel(item.kind)}</span>
          <input
            bind:value={item.title}
            maxlength="200"
            aria-label="Title"
            placeholder={item.kind === "scripture" ? "Reference, e.g. John 3:16" : "Title"}
          />
          <span class="count">{built.items[i]?.slides.length ?? 0} slides</span>
          <button class="icon" onclick={() => move(i, -1)} disabled={i === 0} aria-label="Move up">↑</button>
          <button class="icon" onclick={() => move(i, 1)} disabled={i === draft.items.length - 1} aria-label="Move down"
            >↓</button
          >
          <button class="icon" onclick={() => remove(i)} aria-label="Remove">✕</button>
        </div>
        {#if item.kind === "image"}
          <div class="thumbs">
            {#each item.images ?? [] as image, n (n)}
              <div class="thumb">
                <Thumb id={image} />
                <button
                  class="icon remove"
                  aria-label="Remove image"
                  onclick={() => (item.images = item.images?.filter((_, k) => k !== n))}>✕</button
                >
              </div>
            {/each}
          </div>
          <label class="pick">
            <input
              type="file"
              accept="image/*"
              multiple
              onchange={(e) => {
                const input = e.currentTarget;
                void addImages(item, input.files).then(() => (input.value = ""));
              }}
            />
            + Add pictures
          </label>
          {#if adding}<span class="hint">{adding}</span>{/if}
          <p class="hint">Each picture is one slide, shown filling the screen.</p>
        {:else if item.kind === "countdown"}
          <div class="row">
            <label>Text <input bind:value={item.text} maxlength="200" /></label>
            <label>Counts down to <input type="time" bind:value={item.at} /></label>
          </div>
        {:else}
          {#if item.kind === "loop"}
            <div class="row">
              <label
                >Seconds on each slide <input type="number" min="1" max="3600" bind:value={item.seconds} /></label
              >
            </div>
            <p class="hint">Moves on by itself and starts over after the last slide, until you press Next.</p>
          {/if}
          <textarea
            bind:value={item.text}
            rows="6"
            placeholder={item.kind === "song"
              ? "Paste the lyrics. A blank line starts a new slide."
              : item.kind === "scripture"
                ? "Paste the passage. It is split into slides for you."
                : item.kind === "loop"
                  ? "Welcome, announcements, coffee… A blank line starts a new slide."
                  : "A blank line starts a new slide."}
          ></textarea>
        {/if}
      </article>
    {:else}
      <p class="empty">Add the songs, readings and countdown for the service.</p>
    {/each}

    <div class="add">
      {#each KINDS as k (k.kind)}
        <button class="quiet" onclick={() => add(k.kind)}>+ {k.label}</button>
      {/each}
    </div>

    <div class="send">
      <button class="primary" onclick={send} disabled={!ready}>Send to screens</button>
      <span class="hint">
        {#if !ready}Connect to send.
        {:else if sending}{sending}
        {:else if sendError}{sendError}
        {:else if unsent}Changes not on the screens yet.
        {:else}On the screens.{/if}
      </span>
    </div>
  </section>

  <section class="live" aria-label="Live">
    <div class="screen" class:black={live?.black}>
      {#if live?.black}
        <span class="note">Screens are black</span>
      {:else if live?.slide}
        {#if live.slide.image}<Thumb id={live.slide.image} fill />{/if}
        {#each live.slide.lines as line, i (i)}<div class="over">{line}</div>{/each}
        {#if live.slide.countdownTo}<div class="note">Countdown</div>{/if}
      {:else if live}
        <span class="note">Slide {live.slideIndex + 1}</span>
      {:else}
        <span class="note">Not connected</span>
      {/if}
    </div>

    <div class="controls">
      <button onclick={() => go("slide.prev")} disabled={!ready}>Back</button>
      <button class="primary" onclick={() => go("slide.next")} disabled={!ready}>Next</button>
      <button onclick={black} disabled={!ready} class:on={live?.black}>Black</button>
    </div>
    <p class="hint center">
      {#if live?.slideCount}Slide {current + 1} of {live.slideCount} · {/if}← → or Space to move, B for black
    </p>

    {#if onScreens}
      <ol class="slides">
        {#each slides as s, i (i)}
          {#if i === 0 || slides[i - 1]?.itemIndex !== s.itemIndex}
            <li class="group">
              {s.item.title || kindLabel(s.item.kind)}{#if s.item.advanceSeconds}{` · loops every ${s.item.advanceSeconds} s`}{/if}
            </li>
          {/if}
          <li>
            <button class="slide" class:current={i === current} onclick={() => goto(i)} disabled={!ready}>
              {s.slide.lines.join(" / ") || (s.slide.image ? "(picture)" : "(blank)")}
            </button>
          </li>
        {/each}
      </ol>
    {/if}
  </section>
</main>

<style>
  header {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 12px 20px;
    padding: 12px 20px;
    background: var(--navy);
    color: var(--paper);
  }
  /* The lockup carries its own navy background, the same as this header. */
  .brand {
    display: block;
    height: 44px;
    width: auto;
    margin: -6px 0;
  }
  .connect {
    display: flex;
    align-items: center;
    gap: 8px;
    flex: 1;
    min-width: 260px;
  }
  .connect label {
    font-size: 13px;
    opacity: 0.8;
  }
  .connect input {
    flex: 1;
    min-width: 0;
    max-width: 280px;
    background: rgba(255, 255, 255, 0.1);
    border: 1px solid rgba(255, 255, 255, 0.25);
    color: var(--paper);
    border-radius: 6px;
    padding: 6px 10px;
  }
  .status {
    font-size: 13px;
    padding: 4px 10px;
    border-radius: 999px;
    background: rgba(255, 255, 255, 0.12);
  }
  .status[data-kind="ready"]::before {
    content: "";
    display: inline-block;
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: #5fd38d;
    margin-right: 6px;
  }
  .banner {
    margin: 16px 20px 0;
    padding: 14px 18px;
    background: var(--card);
    border: 1px solid var(--line);
    border-left: 4px solid var(--blue);
    border-radius: 8px;
  }
  .banner p {
    margin: 0 0 8px;
  }
  .banner form {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
    align-items: center;
  }
  .code {
    font-size: 32px;
    font-weight: 700;
    letter-spacing: 0.2em;
    font-variant-numeric: tabular-nums;
  }
  main {
    display: grid;
    grid-template-columns: minmax(0, 1fr) minmax(0, 1fr);
    gap: 20px;
    padding: 20px;
    max-width: 1400px;
    margin: 0 auto;
  }
  @media (max-width: 860px) {
    main {
      grid-template-columns: minmax(0, 1fr);
      padding: 16px;
    }
    .live {
      order: -1;
    }
  }
  button {
    border: 1px solid var(--line);
    background: var(--card);
    border-radius: 6px;
    padding: 7px 14px;
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
  button.quiet {
    background: transparent;
  }
  button.icon {
    padding: 4px 8px;
    background: transparent;
    border-color: transparent;
  }
  input,
  textarea {
    border: 1px solid var(--line);
    border-radius: 6px;
    padding: 6px 10px;
    background: #fff;
  }
  textarea {
    width: 100%;
    resize: vertical;
    line-height: 1.4;
  }
  .plan-head {
    display: flex;
    gap: 8px;
    margin-bottom: 12px;
  }
  .sheet-title {
    flex: 1;
    font-size: 20px;
    font-weight: 700;
    background: transparent;
    border-color: transparent;
    padding-left: 0;
  }
  .sheet-title:focus {
    border-color: var(--line);
    background: #fff;
    padding-left: 10px;
  }
  .item {
    background: var(--card);
    border: 1px solid var(--line);
    border-radius: 8px;
    padding: 10px 12px 12px;
    margin-bottom: 10px;
  }
  .item-head {
    display: flex;
    align-items: center;
    gap: 6px;
    margin-bottom: 8px;
  }
  .item-head input {
    flex: 1;
    min-width: 0;
    font-weight: 600;
  }
  .kind {
    font-size: 12px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: var(--blue);
    min-width: 76px;
  }
  .count {
    font-size: 12px;
    color: var(--muted);
    white-space: nowrap;
  }
  .row {
    display: flex;
    flex-wrap: wrap;
    gap: 12px;
  }
  .row input[type="number"] {
    width: 90px;
  }
  .row label {
    display: flex;
    flex-direction: column;
    gap: 4px;
    font-size: 13px;
    color: var(--muted);
  }
  .empty,
  .hint {
    color: var(--muted);
    font-size: 13px;
  }
  .add {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    margin: 4px 0 16px;
  }
  .send {
    display: flex;
    align-items: center;
    gap: 12px;
    position: sticky;
    bottom: 0;
    padding: 12px 0;
    background: var(--paper);
  }
  .thumbs {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(120px, 1fr));
    gap: 8px;
    margin-bottom: 8px;
  }
  .thumb {
    position: relative;
    aspect-ratio: 16 / 9;
    border-radius: 4px;
    overflow: hidden;
  }
  .thumb .remove {
    position: absolute;
    top: 2px;
    right: 2px;
    background: rgba(16, 34, 79, 0.75);
    color: #fff;
    border-radius: 4px;
  }
  .pick {
    display: inline-block;
    border: 1px dashed var(--line);
    border-radius: 6px;
    padding: 7px 14px;
    cursor: pointer;
  }
  .pick input {
    position: absolute;
    width: 1px;
    height: 1px;
    opacity: 0;
  }
  .pick:focus-within {
    outline: 2px solid var(--blue);
  }
  .over {
    position: relative;
    text-shadow: 0 0 0.25em #000;
  }
  .screen {
    aspect-ratio: 16 / 9;
    background: #000;
    color: #fff;
    border-radius: 8px;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    text-align: center;
    font-weight: 700;
    font-size: clamp(14px, 2.2vw, 26px);
    line-height: 1.25;
    padding: 0 6%;
    border: 3px solid var(--navy);
    position: relative;
    overflow: hidden;
  }
  .screen.black {
    border-color: var(--orange);
  }
  .note {
    font-size: 14px;
    font-weight: 400;
    opacity: 0.6;
  }
  .controls {
    display: grid;
    grid-template-columns: 1fr 2fr 1fr;
    gap: 8px;
    margin-top: 12px;
  }
  .controls button {
    padding: 14px;
    font-size: 17px;
  }
  .controls button.on {
    border-color: var(--orange);
    color: var(--orange);
    font-weight: 600;
  }
  .center {
    text-align: center;
  }
  .slides {
    list-style: none;
    padding: 0;
    margin: 8px 0 0;
  }
  .group {
    font-size: 12px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: var(--muted);
    margin: 12px 0 4px;
  }
  .slide {
    width: 100%;
    text-align: left;
    margin-bottom: 4px;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .slide.current {
    border-color: var(--blue);
    box-shadow: inset 4px 0 0 var(--blue);
    font-weight: 600;
  }
</style>
