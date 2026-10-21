<script lang="ts">
  import { addImage } from "./media";
  import { FONTS, MAX_SIZE, MIN_SIZE, type Theme } from "./theme";

  /**
   * Picks the look of the screens, and makes a new look from the one picked: font, size, text
   * color and a background color or picture. Saving needs the church computer (themes live in
   * its library); picking works from what this page already has.
   */
  let {
    themes,
    selected,
    canSave,
    pick,
    save,
  }: {
    themes: { id: string; title: string; theme: Theme }[];
    selected: string;
    canSave: boolean;
    pick: (id: string) => void;
    /** Saves the edited theme under a title; resolves to a message to show. */
    save: (title: string, theme: Theme) => Promise<string>;
  } = $props();

  const current = $derived(themes.find((t) => t.id === selected) ?? themes[0]!);
  let editing = $state(false);
  let copy = $state<Theme | null>(null);
  let title = $state("");
  let note = $state("");

  function startEdit() {
    copy = structuredClone($state.snapshot(current.theme)) as Theme;
    title = `${current.title} copy`;
    note = "";
    editing = true;
  }
  async function setImage(files: FileList | null) {
    const file = files?.[0];
    if (!copy || !file) return;
    try {
      copy.background = { kind: "image", image: await addImage(file), color: copy.background.color };
    } catch (e) {
      note = e instanceof Error ? e.message : "Couldn't use that picture.";
    }
  }
  function setColor(color: string) {
    if (copy) copy.background = copy.background.kind === "image" ? { ...copy.background, color } : { kind: "color", color };
  }
  async function doSave() {
    if (copy) note = await save(title.trim() || "Untitled theme", $state.snapshot(copy) as Theme);
  }
</script>

<div class="themes">
  <label>
    Look
    <select value={selected} onchange={(e) => pick(e.currentTarget.value)}>
      {#each themes as t (t.id)}<option value={t.id}>{t.title}</option>{/each}
    </select>
  </label>
  <button class="quiet" onclick={() => (editing ? (editing = false) : startEdit())} aria-expanded={editing}>
    {editing ? "Close" : "Customize"}
  </button>
</div>

{#if editing && copy}
  <div class="editor">
    <label>Font
      <select bind:value={copy.font}>{#each FONTS as f (f)}<option value={f}>{f}</option>{/each}</select>
    </label>
    <label>Size <input type="range" min={MIN_SIZE} max={MAX_SIZE} step="0.5" bind:value={copy.size} /></label>
    <label><input type="checkbox" bind:checked={copy.bold} /> Bold</label>
    <label><input type="checkbox" bind:checked={copy.shadow} /> Shadow</label>
    <label>Text <input type="color" bind:value={copy.text} /></label>
    <label>Background
      <input type="color" value={copy.background.color} oninput={(e) => setColor(e.currentTarget.value)} />
    </label>
    <label class="pick">
      {copy.background.kind === "image" ? "Change picture" : "Use a picture"}
      <input type="file" accept="image/*" onchange={(e) => void setImage(e.currentTarget.files)} />
    </label>
    {#if copy.background.kind === "image"}
      <button class="quiet" onclick={() => copy && (copy.background = { kind: "color", color: copy.background.color })}>No picture</button>
    {/if}
    <label>Name <input bind:value={title} maxlength="200" /></label>
    <button class="primary" onclick={doSave} disabled={!canSave}>Save theme</button>
    <span class="hint">{canSave ? note : "Connect to save."}</span>
  </div>
{/if}

<style>
  .themes,
  .editor {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 10px 14px;
    font-size: 14px;
  }
  .editor {
    margin-top: 8px;
  }
  label {
    display: inline-flex;
    align-items: center;
    gap: 6px;
  }
  .pick {
    border: 1px dashed var(--line);
    border-radius: 6px;
    padding: 4px 10px;
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
</style>
