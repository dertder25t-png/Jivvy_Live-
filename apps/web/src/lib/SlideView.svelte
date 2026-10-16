<script lang="ts">
  import type { Slide } from "@jivvy/protocol";
  import Thumb from "./Thumb.svelte";
  import { themeStyle, type Theme } from "./theme";

  /**
   * One slide drawn in a theme: its background (a picture slide shows its own picture instead),
   * then its lines. Fills the screen box it is placed in, which must be positioned.
   */
  let { slide, theme }: { slide: Slide; theme: Theme } = $props();

  let text = $state<HTMLElement>();
  // A big size or many lines is shrunk until everything fits, never cut off (as the output windows do).
  $effect(() => {
    void [slide, theme];
    const el = text;
    if (!el) return;
    el.style.fontSize = "";
    let size = parseFloat(getComputedStyle(el).fontSize);
    while ((el.scrollHeight > el.clientHeight || el.scrollWidth > el.clientWidth) && size > 1) {
      size *= 0.92;
      el.style.fontSize = `${size}px`;
    }
  });
</script>

<div class="themed" style={themeStyle(theme)}>
  {#if slide.image}
    <Thumb id={slide.image} fill />
  {:else if theme.background.kind === "image"}
    <Thumb id={theme.background.image} fill />
  {/if}
  <div class="text" bind:this={text}>
    {#each slide.lines as line, i (i)}<div class="line">{line}</div>{/each}
    {#if slide.countdownTo}<div class="note">Countdown</div>{/if}
  </div>
</div>

<style>
  .themed {
    position: absolute;
    inset: 0;
    container-type: inline-size;
    background: var(--t-bg);
    color: var(--t-text);
  }
  /* Sized inside the container: container units on the container itself would not see it. */
  .text {
    position: absolute;
    inset: 0;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    padding: 0 6%;
    text-align: center;
    font-family: var(--t-font);
    font-weight: var(--t-weight);
    font-size: var(--t-size);
    line-height: 1.25;
  }
  .line {
    text-shadow: var(--t-shadow);
  }
  .note {
    font-size: 14px;
    font-weight: 400;
    opacity: 0.6;
  }
</style>
