<script lang="ts">
  import { runSheetSlides, type DocInfo, type Levels, type RunSheet, type StateSnapshot } from "@jivvy/protocol";
  import { onDestroy, onMount } from "svelte";
  import { DaemonConnection, type Status } from "$lib/daemon";
  import { addImage, upload } from "$lib/media";
  import { addPdf } from "$lib/pdf";
  import type { Hymn } from "@jivvy/songs/hymns";
  import { build, imagesIn, newId, newItem, songOf, type Draft, type DraftItem, type ItemKind } from "$lib/runsheet";
  import { getDoc, listDocs, newSongId, planFromDoc, saveDoc, songFromDoc } from "$lib/library";
  import { ACTIVE_THEME_ID, BUILT_IN, DEFAULT_THEME_ID, activeBody, activeFromDoc, newThemeId, themeBody, themeFromDoc, type Theme } from "$lib/theme";
  import SlideView from "$lib/SlideView.svelte";
  import ThemePicker from "$lib/ThemePicker.svelte";
  import { LOCK_KEY, isLocked, lock, unlock, validPin } from "$lib/lock";
  import Operator from "$lib/Operator.svelte";
  import SongEditor from "$lib/SongEditor.svelte";
  import Thumb from "$lib/Thumb.svelte";

  const DRAFT_KEY = "jivvy.draft";
  const ADDRESS_KEY = "jivvy.address";
  const VIEW_KEY = "jivvy.view";
  const KINDS: { kind: ItemKind; label: string }[] = [
    { kind: "song", label: "Song" },
    { kind: "scripture", label: "Scripture" },
    { kind: "countdown", label: "Countdown" },
    { kind: "loop", label: "Pre-service loop" },
    { kind: "image", label: "Images" },
    { kind: "pdf", label: "PDF slides" },
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

  const saved = stored<Draft>(DRAFT_KEY, { id: newId(), title: "Sunday service", items: [] });
  // Songs saved before the song editor get their sections from their lyrics, once.
  for (const item of saved.items) if (item.kind === "song" && !item.song) item.song = songOf(item);
  let draft = $state<Draft>(saved);
  let address = $state(stored(ADDRESS_KEY, "ws://127.0.0.1:47801"));
  let deviceName = $state("Run sheet (browser)");
  let status = $state<Status>({ kind: "offline", message: "Not connected." });
  let live = $state<StateSnapshot | null>(null);
  let onScreens = $state<RunSheet | null>(null);
  let sendError = $state("");
  let sending = $state("");
  let conn: DaemonConnection | null = null;

  $effect(() => store(DRAFT_KEY, draft));

  /** Edit builds the run sheet; Operate runs the service. A locked browser only operates. */
  let view = $state<"edit" | "operate">(stored(VIEW_KEY, "edit"));
  $effect(() => store(VIEW_KEY, view));
  let locked = $state(isLocked());
  /** Locked or unlocked in another tab of this browser: follow it at once. */
  function lockChanged(e: StorageEvent) {
    if (e.key !== LOCK_KEY && e.key !== null) return; // null: storage cleared
    locked = isLocked();
    pinForm = "";
  }
  let levels = $state<Levels | null>(null);
  let levelsAt = $state(0);

  // Locking (choose a PIN twice) and unlocking (enter it), each a small form in the header.
  let pinForm = $state<"" | "lock" | "unlock">("");
  let pin = $state("");
  let pinAgain = $state("");
  let pinError = $state("");
  function openPin(kind: "lock" | "unlock") {
    pinForm = pinForm === kind ? "" : kind;
    pin = pinAgain = pinError = "";
  }
  async function submitPin() {
    pinError = "";
    if (pinForm === "lock") {
      if (!validPin(pin)) return void (pinError = "Choose 4 to 8 digits.");
      if (pin !== pinAgain) return void (pinError = "The two PINs don't match.");
      await lock(pin);
      locked = true;
    } else {
      const result = await unlock(pin);
      if (!result.ok) return void (pinError = result.message);
      locked = false;
    }
    pinForm = "";
    pin = pinAgain = "";
  }

  // Countdowns are built for the moment of sending; `builtAt` moves on each send so the
  // comparison below uses the same build that was sent.
  let builtAt = $state(new Date());
  const built = $derived(build(draft, builtAt));
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
      levels: (l) => {
        levels = l;
        levelsAt = Date.now();
      },
      docChanged: (d) => docChanged(d),
    });
    conn.connect();
  }
  // Back on the church computer used last time, straight away. A page the church computer
  // served itself (no internet needed) connects to the address it came from.
  onMount(() => {
    if (stored<string | null>(ADDRESS_KEY, null)) return connect();
    void servedByDaemon().then((own) => {
      if (!own || stored<string | null>(ADDRESS_KEY, null)) return;
      address = own;
      connect();
    });
  });

  /** This page's own address as a channel, if a church computer served it; else null. */
  async function servedByDaemon(): Promise<string | null> {
    try {
      const res = await fetch("/.well-known/jivvy", { cache: "no-store" });
      const info = res.ok ? ((await res.json()) as { service?: string }) : null;
      if (info?.service !== "jivvy-live") return null;
      return `${location.protocol === "https:" ? "wss" : "ws"}://${location.host}`;
    } catch {
      return null; // app.jivvy.org answers with the app's own page, which isn't JSON
    }
  }
  onDestroy(() => conn?.close());

  /** One send at a time: a second one mid-upload would restart the same images and race the first. */
  let inFlight = $state(false);
  async function send() {
    if (!conn || inFlight) return;
    inFlight = true;
    sendError = "";
    try {
      // Built now, not the copy kept for the editor: a countdown whose time has passed while
      // the page was open counts down to the next one, not to a moment already gone.
      builtAt = new Date();
      const sheet = build(draft, builtAt);
      // Images first: the church computer refuses a run sheet showing one it doesn't have.
      const failed = await upload(conn, imagesIn(sheet), (done, total) => {
        sending = total ? `Sending images ${done} of ${total}…` : "";
      });
      if (failed) return void (sendError = failed);
      const ack = await conn.send({ type: "runsheet.set", runSheet: sheet });
      if (!ack.ok) sendError = ack.message;
    } finally {
      sending = "";
      inFlight = false;
    }
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

  /**
   * A PDF item holds one document: picking another replaces its pages. Only the newest pick
   * for an item counts, so a slow earlier one can't land on top of it.
   */
  const pdfPicks = new Map<string, number>();
  async function addPdfTo(item: DraftItem, file: File | undefined) {
    if (!file) return;
    const pick = (pdfPicks.get(item.id) ?? 0) + 1;
    pdfPicks.set(item.id, pick);
    const newest = () => pdfPicks.get(item.id) === pick;
    sendError = "";
    try {
      const pages = await addPdf(file, (page, total) => {
        if (newest()) adding = `Reading page ${page} of ${total}…`;
      });
      if (!newest()) return;
      item.images = pages;
      const name = file.name.replace(/\.pdf$/i, "");
      // Named after the file unless someone has typed a title of their own.
      if (!item.title || item.title === "PDF slides" || item.title === item.titleFrom) item.title = name;
      item.titleFrom = name;
    } catch (e) {
      if (newest()) sendError = e instanceof Error ? e.message : String(e);
    } finally {
      if (newest()) adding = "";
    }
  }

  const go = (type: "slide.next" | "slide.prev") => conn?.send({ type });
  const goto = (index: number) => conn?.send({ type: "slide.goto", index });
  const black = () => conn?.send({ type: "output.black", on: !live?.black });

  function add(kind: ItemKind) {
    draft.items.push(newItem(kind));
  }

  /** The public-domain hymn library, loaded the first time it is opened. */
  let hymns = $state<Hymn[] | null>(null);
  let hymnsOpen = $state(false);
  let hymnSearch = $state("");
  let hymnSong: ((h: Hymn) => DraftItem["song"]) | null = null;
  const hymnMatches = $derived(
    (hymns ?? []).filter((h) => `${h.title} ${h.author}`.toLowerCase().includes(hymnSearch.trim().toLowerCase())),
  );
  async function openHymns() {
    hymnsOpen = !hymnsOpen;
    if (hymnsOpen) void refreshSaved();
    if (hymns) return;
    try {
      const lib = await import("@jivvy/songs/hymns");
      hymnSong = lib.hymnSong;
      hymns = lib.HYMNS;
    } catch (e) {
      sendError = e instanceof Error ? e.message : String(e);
    }
  }
  function addHymn(h: Hymn) {
    const song = hymnSong?.(h);
    if (!song) return;
    draft.items.push({ ...newItem("song"), title: h.title, song });
    hymnsOpen = false;
    hymnSearch = "";
  }

  // ---- The church computer's library: saved songs and plans ----

  /** Songs saved on the church computer (shown with the hymns), while the picker is open. */
  let savedSongs = $state<DocInfo[]>([]);
  const savedMatches = $derived(savedSongs.filter((s) => s.title.toLowerCase().includes(hymnSearch.trim().toLowerCase())));
  async function refreshSaved() {
    if (!conn || !ready) return void (savedSongs = []);
    try {
      savedSongs = await listDocs(conn, "song");
    } catch {
      savedSongs = []; // an older church computer without a library: hymns only
    }
  }
  async function addSaved(info: DocInfo) {
    if (!conn) return;
    try {
      const doc = await getDoc(conn, "song", info.id);
      const song = songFromDoc(doc);
      const libraryDaemon = conn.daemonId ?? undefined;
      draft.items.push({ ...newItem("song"), title: doc.title, song, libraryId: doc.id, libraryRev: doc.rev, libraryDaemon });
      hymnsOpen = false;
      hymnSearch = "";
    } catch (e) {
      sendError = e instanceof Error ? e.message : String(e);
    }
  }

  /** What happened to each song's last save to the library, by item id. */
  let songNotes = $state<Record<string, string>>({});
  /** Songs with a save on its way: a second one would be based on the same revision and be refused. */
  let songSaving = $state<Record<string, boolean>>({});
  async function saveSong(item: DraftItem) {
    if (!conn || !item.song || songSaving[item.id]) return;
    songSaving[item.id] = true;
    try {
      await saveSongNow(conn, item);
    } finally {
      songSaving[item.id] = false;
    }
  }
  async function saveSongNow(conn: DaemonConnection, item: DraftItem) {
    if (!item.song) return;
    const id = item.libraryId ?? newSongId(item.title);
    const title = item.title.trim() || "Untitled song";
    const body = { ...item.song, title } as unknown as Record<string, unknown>;
    // A revision from another church computer means nothing here: this one gets it as new.
    const here = conn.daemonId ?? undefined;
    const baseRev = item.libraryDaemon === here ? (item.libraryRev ?? 0) : 0;
    const result = await saveDoc(conn, { kind: "song", id, title, body }, baseRev);
    if (result.ok) {
      item.libraryId = id;
      item.libraryRev = result.rev;
      item.libraryDaemon = here;
      songNotes[item.id] = "Saved to the church computer's library.";
    } else {
      songNotes[item.id] = result.conflict
        ? "Someone changed this song on another device since you added it. Add it again from the library to get their version."
        : result.message;
    }
  }

  /**
   * Plans: this run sheet is kept on the church computer as you edit. The revision each save
   * was from, by church computer and plan id: another church computer has its own copies.
   */
  const PLAN_REVS_KEY = "jivvy.planRevs.v2";
  const planKey = (id: string) => `${conn?.daemonId ?? "?"}/${id}`;
  let planRevs = $state<Record<string, number>>(stored(PLAN_REVS_KEY, {}));
  $effect(() => store(PLAN_REVS_KEY, planRevs));
  let planNote = $state("");
  /** A plan changed elsewhere stops saving here until it is opened again, so neither edit is lost. */
  let planHeld = $state("");
  let planTimer: ReturnType<typeof setTimeout> | undefined;
  $effect(() => {
    const items = JSON.stringify(draft.items); // re-run on every edit
    const { id, title } = draft;
    if (!ready || !conn || planHeld === planKey(id)) return;
    clearTimeout(planTimer);
    planTimer = setTimeout(() => queuePlanSave({ id, title, items }), 1500);
    return () => clearTimeout(planTimer);
  });
  /**
   * One plan save at a time: a second one sent before the first is acknowledged would be based
   * on the same revision and be refused as if another device had changed the plan. Edits made
   * meanwhile wait, and only the newest of them is sent next: the newest for each plan, so
   * switching to another plan never drops the last edits of the one before.
   */
  const planPending = new Map<string, { title: string; items: string }>();
  /** Before this browser moves to another plan: the current one's last edit goes now, not after the wait. */
  function saveCurrentPlanNow() {
    clearTimeout(planTimer);
    if (ready && conn && planHeld !== planKey(draft.id))
      queuePlanSave({ id: draft.id, title: draft.title, items: JSON.stringify(draft.items) });
  }
  let planSaving = false;
  function queuePlanSave(plan: { id: string; title: string; items: string }) {
    planPending.delete(plan.id); // re-added last: plans save in the order they were edited
    planPending.set(plan.id, { title: plan.title, items: plan.items });
    if (!planSaving) void flushPlanSaves();
  }
  async function flushPlanSaves() {
    planSaving = true;
    try {
      for (let next = planPending.entries().next(); !next.done; next = planPending.entries().next()) {
        const [id, plan] = next.value;
        planPending.delete(id);
        await savePlan(id, plan.title, plan.items);
      }
    } finally {
      planSaving = false;
    }
  }
  async function savePlan(id: string, title: string, items: string) {
    const key = planKey(id);
    if (!conn || planHeld === key) return;
    const body = { items: JSON.parse(items) as unknown };
    const result = await saveDoc(conn, { kind: "plan", id, title: title.trim() || "Untitled plan", body }, planRevs[key] ?? 0);
    if (result.ok) {
      planRevs[key] = result.rev;
      planNote = "Saved on the church computer.";
    } else if (result.conflict) {
      planHeld = key;
      planNote = "This plan was changed on another device. Open it again from Plans to continue; your changes stay in this browser.";
    } else {
      planNote = result.message;
    }
  }
  let plansOpen = $state(false);
  let plans = $state<DocInfo[]>([]);
  async function openPlans() {
    plansOpen = !plansOpen;
    if (!plansOpen || !conn) return;
    try {
      plans = await listDocs(conn, "plan");
    } catch (e) {
      plans = [];
      planNote = e instanceof Error ? e.message : String(e);
    }
  }
  async function openPlan(info: DocInfo) {
    if (!conn) return;
    try {
      const doc = await getDoc(conn, "plan", info.id);
      const plan = planFromDoc(doc);
      if (!plan) return void (planNote = "That plan was saved by a newer version of Jivvy Live.");
      if (doc.id !== draft.id) saveCurrentPlanNow();
      planRevs[planKey(doc.id)] = doc.rev;
      if (planHeld === planKey(doc.id)) planHeld = "";
      draft = plan;
      plansOpen = false;
      planNote = "";
    } catch (e) {
      planNote = e instanceof Error ? e.message : String(e);
    }
  }
  // ---- Themes: the look of the screens, one for the whole service ----

  let savedThemes = $state<{ id: string; title: string; theme: Theme }[]>([]);
  /** The theme in use, kept as a library setting so every device (and later the screens) share it. */
  let themeId = $state(DEFAULT_THEME_ID);
  let themesLoaded = $state(false);
  async function refreshActive() {
    if (!conn || !ready) return;
    try {
      const id = activeFromDoc(await getDoc(conn, "setting", ACTIVE_THEME_ID));
      if (id) themeId = id;
    } catch {
      // nothing chosen yet, or an older church computer: the default
    }
  }
  async function pickTheme(id: string) {
    themeId = id;
    if (conn) await saveDoc(conn, { kind: "setting", id: ACTIVE_THEME_ID, title: "Active theme", body: activeBody(id) });
  }
  // A theme that was deleted or never existed here falls back to the default.
  $effect(() => {
    if (themesLoaded && !themes.some((t) => t.id === themeId)) themeId = DEFAULT_THEME_ID;
  });
  const themes = $derived([...BUILT_IN, ...savedThemes]);
  const theme = $derived((themes.find((t) => t.id === themeId) ?? BUILT_IN.find((t) => t.id === DEFAULT_THEME_ID)!).theme);
  async function refreshThemes() {
    if (!conn || !ready) return;
    try {
      const found = [];
      for (const info of await listDocs(conn, "theme")) {
        const t = themeFromDoc(await getDoc(conn, "theme", info.id));
        if (t) found.push({ id: info.id, title: info.title, theme: t });
      }
      savedThemes = found;
      themesLoaded = true;
    } catch {
      savedThemes = []; // an older church computer without a library: the built-in themes
    }
  }
  $effect(() => {
    if (ready) void refreshThemes().then(refreshActive);
  });
  async function saveTheme(title: string, t: Theme): Promise<string> {
    if (!conn) return "Not connected.";
    const id = newThemeId(title);
    const result = await saveDoc(conn, { kind: "theme", id, title, body: themeBody(t) }, 0);
    if (!result.ok) return result.message;
    await refreshThemes();
    themeId = id;
    return "Saved to the church computer's library.";
  }

  /** Another device saved or deleted something: open lists follow it. */
  function docChanged(doc: DocInfo) {
    if (doc.kind === "theme") void refreshThemes();
    if (doc.kind === "setting" && doc.id === ACTIVE_THEME_ID) void refreshActive();
    if (doc.kind === "song" && hymnsOpen) void refreshSaved();
    if (doc.kind === "plan" && plansOpen && conn) void listDocs(conn, "plan").then((p) => (plans = p), () => {});
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
    saveCurrentPlanNow();
    draft = { id: newId(), title: "Sunday service", items: [] };
  }

  /** Arrow keys, Page Up/Down and Space move the slides, except while typing. */
  function keydown(e: KeyboardEvent) {
    const t = e.target as HTMLElement;
    if (!ready || t.closest("input, textarea, select, [contenteditable]")) return;
    // Space presses a focused button (Back, Black, a slide in the list), as it should.
    if (e.key === " " && t.closest("button, a, summary, [role=button]")) return;
    if (["ArrowRight", "ArrowDown", "PageDown", " "].includes(e.key)) go("slide.next");
    else if (["ArrowLeft", "ArrowUp", "PageUp"].includes(e.key)) go("slide.prev");
    else if (e.key === "b" || e.key === "B") black();
    else return;
    e.preventDefault();
  }

  const kindLabel = (k: string) => KINDS.find((x) => x.kind === k)?.label ?? "Text";
</script>

<svelte:window onkeydown={keydown} onstorage={lockChanged} />

<header>
  <img class="brand" src="/brand/jivvy-live-lockup.svg" alt="Jivvy Live" width="147" height="60" />
  {#if locked}
    <span class="mode">Volunteer mode</span>
    <span class="spacer"></span>
  {:else}
    <nav class="tabs" aria-label="View">
      <button class:active={view === "edit"} aria-pressed={view === "edit"} onclick={() => (view = "edit")}>Edit</button>
      <button class:active={view === "operate"} aria-pressed={view === "operate"} onclick={() => (view = "operate")}
        >Operate</button
      >
    </nav>
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
  {/if}
  <button class="lock" onclick={() => openPin(locked ? "unlock" : "lock")} aria-expanded={pinForm !== ""}>
    {locked ? "Unlock" : "Lock for volunteers"}
  </button>
  <div class="status" data-kind={status.kind} role="status">
    {#if status.kind === "ready"}Connected
    {:else if status.kind === "connecting"}Connecting…
    {:else if status.kind === "pairing"}Waiting for approval
    {:else if status.kind === "unpaired"}Not paired
    {:else}{status.message}{/if}
  </div>
</header>

{#if pinForm}
  <form
    class="banner pin"
    onsubmit={(e) => {
      e.preventDefault();
      void submitPin();
    }}
  >
    <p>
      {#if pinForm === "lock"}
        Volunteer mode shows only the operator view: no editing, no settings, no connecting elsewhere.
        Choose a PIN to leave it; it is kept on this device only.
      {:else}
        Enter the PIN this device was locked with.
      {/if}
    </p>
    <div class="row">
      <label
        >PIN <input
          type="password"
          inputmode="numeric"
          autocomplete="off"
          maxlength="8"
          aria-label="PIN"
          bind:value={pin}
          required
        /></label
      >
      {#if pinForm === "lock"}
        <label
          >Again <input
            type="password"
            inputmode="numeric"
            autocomplete="off"
            maxlength="8"
            aria-label="PIN again"
            bind:value={pinAgain}
            required
          /></label
        >
      {/if}
      <button type="submit" class="primary">{pinForm === "lock" ? "Lock" : "Unlock"}</button>
      <button type="button" class="quiet" onclick={() => (pinForm = "")}>Cancel</button>
    </div>
    {#if pinError}<p class="error" role="alert">{pinError}</p>{/if}
  </form>
{/if}

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

{#if locked || view === "operate"}
  <Operator {theme} {live} {onScreens} {ready} {levels} {levelsAt} go={(t) => void go(t)} goto={(i) => void goto(i)} black={() => void black()} />
{:else}
<main>
  <section class="plan" aria-label="Run sheet">
    <div class="plan-head">
      <input class="sheet-title" bind:value={draft.title} aria-label="Run sheet title" maxlength="200" />
      <button class="quiet" onclick={openPlans} disabled={!ready} aria-expanded={plansOpen}>Plans</button>
      <button class="quiet" onclick={startNew}>New</button>
    </div>
    {#if plansOpen}
      <div class="hymns">
        <ul>
          {#each plans as p (p.id)}
            <li>
              <button class="hymn" onclick={() => openPlan(p)}>
                <strong>{p.title || "Untitled plan"}</strong>
                <span class="hint">saved {new Date(p.updatedAt).toLocaleString()}</span>
              </button>
            </li>
          {:else}
            <li class="hint">No plans on the church computer yet. This one is saved there as you edit.</li>
          {/each}
        </ul>
      </div>
    {/if}
    {#if planNote}<p class="hint">{planNote}</p>{/if}

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
        {#if item.kind === "image" || item.kind === "pdf"}
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
          {#if item.kind === "pdf"}
            <label class="pick">
              <input
                type="file"
                accept="application/pdf,.pdf"
                onchange={(e) => {
                  const input = e.currentTarget;
                  void addPdfTo(item, input.files?.[0]).then(() => (input.value = ""));
                }}
              />
              {item.images?.length ? "Choose another PDF" : "+ Choose a PDF"}
            </label>
            {#if adding}<span class="hint">{adding}</span>{/if}
            <p class="hint">Each page is one slide, drawn here and sent as a picture.</p>
          {:else}
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
          {/if}
        {:else if item.kind === "song" && item.song}
          <SongEditor bind:song={item.song} />
          <div class="row library">
            <button class="quiet" onclick={() => saveSong(item)} disabled={!ready || songSaving[item.id]}
              >{item.libraryId ? "Save changes to the library" : "Save to the library"}</button
            >
            {#if songNotes[item.id]}<span class="hint">{songNotes[item.id]}</span>{/if}
          </div>
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
      <button class="quiet" onclick={openHymns} aria-expanded={hymnsOpen}>+ From the library</button>
    </div>

    {#if hymnsOpen}
      <div class="hymns">
        <input bind:value={hymnSearch} placeholder="Search songs and hymns" aria-label="Search songs and hymns" />
        {#if savedMatches.length}
          <p class="group-label">Saved on the church computer</p>
          <ul>
            {#each savedMatches as s (s.id)}
              <li>
                <button class="hymn" onclick={() => addSaved(s)}><strong>{s.title}</strong></button>
              </li>
            {/each}
          </ul>
          <p class="group-label">Public-domain hymns</p>
        {/if}
        {#if !hymns}
          <p class="hint">Loading…</p>
        {:else}
          <ul>
            {#each hymnMatches as h (h.id)}
              <li>
                <button class="hymn" onclick={() => addHymn(h)}>
                  <strong>{h.title}</strong> <span class="hint">{h.author}, {h.year}</span>
                </button>
              </li>
            {:else}
              <li class="hint">No hymn matches.</li>
            {/each}
          </ul>
          <p class="hint">Public domain, free to project and stream.</p>
        {/if}
      </div>
    {/if}

    <div class="send">
      <button class="primary" onclick={send} disabled={!ready || inFlight}>Send to screens</button>
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
    <ThemePicker {themes} selected={themeId} canSave={ready} pick={(id) => void pickTheme(id)} save={saveTheme} />
    <div class="screen" class:black={live?.black}>
      {#if live?.black}
        <span class="note">Screens are black</span>
      {:else if live?.slide}
        <SlideView slide={live.slide} {theme} />
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
{/if}

<style>
  .tabs {
    display: flex;
    gap: 2px;
    background: rgba(255, 255, 255, 0.1);
    border-radius: 8px;
    padding: 3px;
  }
  .tabs button {
    background: transparent;
    border-color: transparent;
    color: var(--paper);
    padding: 5px 14px;
  }
  .tabs button.active {
    background: var(--paper);
    color: var(--navy);
    font-weight: 600;
  }
  .mode {
    font-weight: 600;
    font-size: 14px;
  }
  .spacer {
    flex: 1;
  }
  button.lock {
    background: transparent;
    color: var(--paper);
    border-color: rgba(255, 255, 255, 0.35);
    font-size: 13px;
  }
  .pin .row {
    align-items: flex-end;
  }
  .pin input {
    width: 130px;
  }
  .error {
    color: #c0392b;
    font-size: 13px;
  }
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
  .hymns {
    background: var(--card);
    border: 1px solid var(--line);
    border-radius: 8px;
    padding: 10px 12px;
    margin-bottom: 16px;
  }
  .group-label {
    font-size: 12px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: var(--muted);
    margin: 10px 0 0;
  }
  .row.library {
    align-items: center;
    margin-top: 6px;
  }
  .hymns input {
    width: 100%;
  }
  .hymns ul {
    list-style: none;
    padding: 0;
    margin: 8px 0 0;
    max-height: 260px;
    overflow-y: auto;
  }
  .hymn {
    width: 100%;
    text-align: left;
    border-color: transparent;
    background: transparent;
  }
  .hymn:hover {
    background: rgba(46, 107, 255, 0.08);
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
