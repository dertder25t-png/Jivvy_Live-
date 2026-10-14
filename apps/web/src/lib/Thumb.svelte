<script lang="ts">
  import { previewUrl } from "./media";

  /** An image kept in this browser, by id; a plain "Image" label if this browser doesn't have it. */
  let { id, fill = false }: { id: string; fill?: boolean } = $props();
  let src = $state<string | null>(null);
  $effect(() => {
    let current = true;
    void previewUrl(id).then((u) => current && (src = u));
    return () => (current = false);
  });
</script>

{#if src}
  <img {src} alt="" class:fill />
{:else}
  <span class="none">Image</span>
{/if}

<style>
  img {
    display: block;
    width: 100%;
    height: 100%;
    object-fit: contain;
    background: #000;
  }
  img.fill {
    position: absolute;
    inset: 0;
  }
  .none {
    font-size: 13px;
    font-weight: 400;
    opacity: 0.6;
  }
</style>
