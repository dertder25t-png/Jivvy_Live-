<script lang="ts">
  import { runSheetSlides, type Levels, type RunSheet, type Slide, type StateSnapshot } from "@jivvy/protocol";
  import Thumb from "./Thumb.svelte";

  /**
   * The operator view: what is on the screens, what comes next, the controls, the slide list
   * with each song's sections, the audio meter, and whether the stream and recording are going.
   * Read-only apart from moving the slides, so it is safe to hand a volunteer.
   */
  let {
    live,
    onScreens,
    ready,
    levels,
    levelsAt,
    go,
    goto,
    black,
  }: {
    live: StateSnapshot | null;
    onScreens: RunSheet | null;
    ready: boolean;
    levels: Levels | null;
    /** When the last levels arrived (ms), to tell a quiet room from a meter that stopped. */
    levelsAt: number;
    go: (type: "slide.next" | "slide.prev") => void;
    goto: (index: number) => void;
    black: () => void;
  } = $props();

  const slides = $derived(onScreens ? runSheetSlides(onScreens) : []);
  const current = $derived(live?.slideIndex ?? 0);
  const now = $derived(slides[current]);
  // Where Next goes, as the daemon decides it: from a pre-service loop, the slide after the
  // whole loop (Next leaves it); otherwise the following slide.
  const nextIndex = $derived.by(() => {
    const here = slides[current];
    if (!here?.item.advanceSeconds) return current + 1;
    let end = current;
    while (slides[end + 1]?.itemIndex === here.itemIndex) end++;
    return end + 1;
  });
  const next = $derived(slides[nextIndex]);

  // Levels arrive about ten a second; none for 2 s means the meter has nothing to show.
  let clock = $state(Date.now());
  $effect(() => {
    const t = setInterval(() => (clock = Date.now()), 500);
    return () => clearInterval(t);
  });
  const meterLive = $derived(levels !== null && clock - levelsAt < 2000);

  /** -60 dBFS (silence) to 0 dBFS (full scale) as a share of the bar. */
  const width = (db: number) => `${Math.round(Math.min(1, Math.max(0, (db + 60) / 60)) * 100)}%`;

  const kindLabel = (k: string) =>
    ({ song: "Song", scripture: "Scripture", countdown: "Countdown", loop: "Pre-service loop", image: "Images", pdf: "PDF slides" })[
      k
    ] ?? "Text";

  const preview = (s: Slide | undefined) => (s ? s.lines.join(" / ") || (s.image ? "(picture)" : "(blank)") : "");

  const streamText = $derived(
    live?.stream === "live" ? "Live" : live?.stream === "reconnecting" ? "Reconnecting…" : "Off",
  );
  const recordingText = $derived(
    live?.recording === "recording" ? "Recording" : live?.recording === "waiting" ? "Waiting…" : "Off",
  );
</script>

<div class="operator">
  <div class="bar">
    <span class="chip" data-on={live?.stream === "live"} data-warn={live?.stream === "reconnecting"}>Stream: {streamText}</span>
    <span class="chip" data-on={live?.recording === "recording"} data-warn={live?.recording === "waiting" || live?.recordingLowDisk}>
      Recording: {recordingText}{#if live?.recordingLowDisk} · disk nearly full{/if}
    </span>
    <div class="meter" aria-label="Audio level" role="meter" aria-valuemin={-60} aria-valuemax={0} aria-valuenow={levels?.peakDb[0] ?? -60}>
      {#if meterLive && levels}
        {#each levels.peakDb as peak, ch (ch)}
          <div class="channel">
            <div class="rms" style:width={width(levels.rmsDb[ch] ?? -60)}></div>
            <div class="peak" class:hot={peak > -6} style:left={width(peak)}></div>
          </div>
        {/each}
      {:else}
        <span class="quiet">No audio level</span>
      {/if}
    </div>
  </div>

  <div class="stage">
    <div class="pane">
      <div class="caption">
        {`On screens${now ? ` · ${now.item.title || kindLabel(now.item.kind)}` : ""}`}{#if now?.slide.label}{" · "}<strong
            >{now.slide.label}</strong
          >{/if}
      </div>
      <div class="screen" class:black={live?.black}>
        {#if live?.black}
          <span class="note">Screens are black</span>
        {:else if live?.slide}
          {#if live.slide.image}<Thumb id={live.slide.image} fill />{/if}
          {#each live.slide.lines as line, i (i)}<div class="over">{line}</div>{/each}
          {#if live.slide.countdownTo}<div class="note">Countdown</div>{/if}
        {:else if live}
          <span class="note">Slide {current + 1}</span>
        {:else}
          <span class="note">Not connected</span>
        {/if}
      </div>
    </div>
    <div class="pane next">
      <div class="caption">
        {["Next", next && (next.item.title || kindLabel(next.item.kind)), next?.slide.label].filter(Boolean).join(" · ")}
      </div>
      <div class="screen small">
        {#if next}
          {#if next.slide.image}<Thumb id={next.slide.image} fill />{/if}
          {#each next.slide.lines as line, i (i)}<div class="over">{line}</div>{/each}
        {:else}
          <span class="note">End of the run sheet</span>
        {/if}
      </div>
    </div>
  </div>

  <div class="controls">
    <button onclick={() => go("slide.prev")} disabled={!ready}>Back</button>
    <button class="primary" onclick={() => go("slide.next")} disabled={!ready}>Next</button>
    <button onclick={black} disabled={!ready} class:on={live?.black}>{live?.black ? "Show" : "Black"}</button>
  </div>
  <p class="hint">
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
            {#if s.slide.label && (i === 0 || slides[i - 1]?.slide.label !== s.slide.label || slides[i - 1]?.itemIndex !== s.itemIndex)}
              <span class="label">{s.slide.label}</span>
            {/if}
            {preview(s.slide)}
          </button>
        </li>
      {/each}
    </ol>
  {:else}
    <p class="hint">No run sheet on the church computer yet.</p>
  {/if}
</div>

<style>
  .operator {
    max-width: 1100px;
    margin: 0 auto;
    padding: 16px 20px 32px;
  }
  .bar {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 8px 12px;
    margin-bottom: 14px;
  }
  .chip {
    font-size: 13px;
    padding: 4px 10px;
    border-radius: 999px;
    border: 1px solid var(--line);
    background: var(--card);
    color: var(--muted);
  }
  .chip[data-on="true"] {
    color: var(--navy);
    border-color: var(--blue);
    font-weight: 600;
  }
  .chip[data-warn="true"] {
    color: var(--orange);
    border-color: var(--orange);
    font-weight: 600;
  }
  .meter {
    flex: 1;
    min-width: 160px;
    display: flex;
    flex-direction: column;
    gap: 3px;
  }
  .channel {
    position: relative;
    height: 7px;
    border-radius: 4px;
    background: rgba(16, 34, 79, 0.12);
    overflow: hidden;
  }
  .rms {
    height: 100%;
    background: var(--blue);
    transition: width 90ms linear;
  }
  .peak {
    position: absolute;
    top: 0;
    width: 3px;
    height: 100%;
    margin-left: -3px;
    background: var(--navy);
  }
  .peak.hot {
    background: var(--orange);
  }
  .quiet {
    font-size: 12px;
    color: var(--muted);
  }
  .stage {
    display: grid;
    grid-template-columns: minmax(0, 2fr) minmax(0, 1fr);
    gap: 16px;
    align-items: end;
  }
  @media (max-width: 700px) {
    .stage {
      grid-template-columns: minmax(0, 1fr);
    }
  }
  .caption {
    font-size: 13px;
    color: var(--muted);
    margin-bottom: 6px;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .caption strong {
    color: var(--blue);
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
    font-size: clamp(14px, 2.4vw, 30px);
    line-height: 1.25;
    padding: 0 6%;
    border: 3px solid var(--navy);
    position: relative;
    overflow: hidden;
  }
  .screen.small {
    font-size: clamp(10px, 1.1vw, 15px);
    border-width: 1px;
    opacity: 0.85;
  }
  .screen.black {
    border-color: var(--orange);
  }
  .over {
    position: relative;
    text-shadow: 0 0 0.25em #000;
  }
  .note {
    font-size: 14px;
    font-weight: 400;
    opacity: 0.6;
  }
  .controls {
    display: grid;
    grid-template-columns: 1fr 2fr 1fr;
    gap: 10px;
    margin-top: 16px;
  }
  .controls button {
    padding: 20px;
    font-size: 20px;
  }
  .controls button.on {
    border-color: var(--orange);
    color: var(--orange);
    font-weight: 600;
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
  .hint {
    color: var(--muted);
    font-size: 13px;
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
    margin: 14px 0 4px;
  }
  .slide {
    width: 100%;
    text-align: left;
    margin-bottom: 4px;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    padding: 9px 14px;
  }
  .slide.current {
    border-color: var(--blue);
    box-shadow: inset 4px 0 0 var(--blue);
    font-weight: 600;
  }
  .label {
    font-size: 11px;
    font-weight: 700;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: var(--blue);
    margin-right: 8px;
  }
</style>
