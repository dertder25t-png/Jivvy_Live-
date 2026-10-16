<script lang="ts">
  import { SECTION_NAMES, nextLabel, parseLyrics, uniqueId, type Song } from "@jivvy/songs";

  /** The song being edited; changes go straight into the run sheet draft. */
  let { song = $bindable() }: { song: Song } = $props();

  const labelOf = (id: string) => song.sections.find((s) => s.id === id)?.label ?? "?";

  // Paste box: open by itself for a new, empty song.
  let pasting = $state(song.sections.length === 0);
  let pasted = $state("");

  function makeSections() {
    if (!pasted.trim()) return;
    if (song.sections.some((s) => s.text.trim()) && !confirm("Replace this song's sections with the pasted lyrics?")) return;
    const parsed = parseLyrics(pasted);
    song.sections = parsed.sections;
    song.arrangement = parsed.arrangement;
    pasted = "";
    pasting = false;
  }

  function addSection(name: string) {
    if (!name) return;
    const label = nextLabel(song, name);
    const id = uniqueId(song.sections, label);
    song.sections.push({ id, label, text: "" });
    song.arrangement.push(id);
  }
  function removeSection(id: string) {
    song.sections = song.sections.filter((s) => s.id !== id);
    song.arrangement = song.arrangement.filter((a) => a !== id);
  }

  function moveBlock(i: number, by: number) {
    const [id] = song.arrangement.splice(i, 1);
    if (id) song.arrangement.splice(i + by, 0, id);
  }
  function removeBlock(i: number) {
    song.arrangement.splice(i, 1);
  }
  function addBlock(id: string) {
    if (id) song.arrangement.push(id);
  }
</script>

<div class="song">
  {#if song.author || song.copyright}
    <p class="credit">{[song.author, song.copyright].filter(Boolean).join(" · ")}</p>
  {/if}

  {#if song.sections.length}
    <div class="arrangement" aria-label="Arrangement">
      <span class="label">Sung as</span>
      {#each song.arrangement as id, i (i)}
        <span class="block">
          <button class="nudge" onclick={() => moveBlock(i, -1)} disabled={i === 0} aria-label="Sing earlier">‹</button>
          {labelOf(id)}
          <button
            class="nudge"
            onclick={() => moveBlock(i, 1)}
            disabled={i === song.arrangement.length - 1}
            aria-label="Sing later">›</button
          >
          <button class="nudge" onclick={() => removeBlock(i)} aria-label="Leave out here">✕</button>
        </span>
      {:else}
        <span class="hint">Nothing yet: add sections below.</span>
      {/each}
      <select
        aria-label="Sing a section again"
        onchange={(e) => {
          addBlock(e.currentTarget.value);
          e.currentTarget.value = "";
        }}
      >
        <option value="">+ Add</option>
        {#each song.sections as s (s.id)}<option value={s.id}>{s.label}</option>{/each}
      </select>
    </div>

    {#each song.sections as section (section.id)}
      <div class="section">
        <div class="section-head">
          <input bind:value={section.label} maxlength="40" aria-label="Section name" />
          <button class="icon" onclick={() => removeSection(section.id)} aria-label="Remove section">✕</button>
        </div>
        <textarea bind:value={section.text} rows={Math.min(8, Math.max(2, section.text.split("\n").length))}></textarea>
      </div>
    {/each}
  {/if}

  <div class="tools">
    <select
      aria-label="Add a section"
      onchange={(e) => {
        addSection(e.currentTarget.value);
        e.currentTarget.value = "";
      }}
    >
      <option value="">+ Section</option>
      {#each SECTION_NAMES as name (name)}<option value={name}>{name}</option>{/each}
    </select>
    <button class="quiet" onclick={() => (pasting = !pasting)}>{pasting ? "Close paste" : "Paste lyrics"}</button>
  </div>

  {#if pasting}
    <div class="paste">
      <textarea
        bind:value={pasted}
        rows="8"
        placeholder={"Paste the whole song. Lines like Verse 1, Chorus or Bridge start sections;\na chorus named again is sung again. Without them, each paragraph is a section."}
      ></textarea>
      <button class="primary" onclick={makeSections} disabled={!pasted.trim()}>Make sections</button>
    </div>
  {/if}
</div>

<style>
  .credit {
    margin: 0 0 8px;
    font-size: 12px;
    color: var(--muted);
  }
  .arrangement {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px;
    margin-bottom: 10px;
  }
  .label {
    font-size: 12px;
    color: var(--muted);
  }
  .block {
    display: inline-flex;
    align-items: center;
    gap: 2px;
    padding: 2px 4px 2px 2px;
    border-radius: 999px;
    background: rgba(46, 107, 255, 0.1);
    color: var(--navy);
    font-size: 13px;
    font-weight: 600;
  }
  .nudge {
    border: 0;
    background: transparent;
    padding: 0 5px;
    font-size: 13px;
    color: var(--blue);
  }
  .section {
    margin-bottom: 8px;
  }
  .section-head {
    display: flex;
    gap: 6px;
    margin-bottom: 4px;
  }
  .section-head input {
    font-size: 12px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: var(--blue);
    border-color: transparent;
    background: transparent;
    padding-left: 0;
    width: 160px;
  }
  .section-head input:focus {
    border-color: var(--line);
    background: #fff;
    padding-left: 10px;
  }
  .tools {
    display: flex;
    gap: 8px;
    flex-wrap: wrap;
  }
  .paste {
    margin-top: 8px;
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 6px;
  }
  select {
    border: 1px solid var(--line);
    border-radius: 6px;
    padding: 5px 8px;
    background: var(--card);
  }
  button {
    border: 1px solid var(--line);
    background: var(--card);
    border-radius: 6px;
    padding: 6px 12px;
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
  button.icon,
  button.nudge {
    border-color: transparent;
    background: transparent;
    padding: 2px 6px;
  }
  textarea {
    width: 100%;
    resize: vertical;
    line-height: 1.4;
    border: 1px solid var(--line);
    border-radius: 6px;
    padding: 6px 10px;
    background: #fff;
  }
  .hint {
    color: var(--muted);
    font-size: 13px;
  }
</style>
