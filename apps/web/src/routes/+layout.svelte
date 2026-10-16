<script lang="ts">
  let { children } = $props();
</script>

<svelte:head><title>Jivvy Live</title></svelte:head>

{@render children()}

<style>
  :global(:root) {
    --navy: #10224f;
    --paper: #f4f1ea;
    --blue: #2e6bff;
    --orange: #ff7a1a;
    --ink: #10224f;
    --muted: #5b6585;
    --line: #d9d4c7;
    --card: #fffdf8;
    --font: "Segoe UI", system-ui, -apple-system, sans-serif;
  }
  :global(*) {
    box-sizing: border-box;
  }
  :global(body) {
    margin: 0;
    background: var(--paper);
    color: var(--ink);
    font-family: var(--font);
    font-size: 15px;
  }
  :global(button),
  :global(input),
  :global(textarea),
  :global(select) {
    font: inherit;
    color: inherit;
  }
</style>
