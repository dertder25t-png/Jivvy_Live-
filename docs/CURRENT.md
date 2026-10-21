# Current work (Stage 1A)

Read this file and your one item only. Do not read the whole repo or the full `docs/BUILD_PLAN.md`.

## Session rules (from the plan)
1. Pick the next unchecked item in the current stage; don't start later stages early.
2. One item per branch and PR, small enough to review in one sitting.
3. Build first, harden later: one or two happy-path tests per feature; chaos tests, timing thresholds and kill loops wait for Stage 1B.
4. Never break the command protocol: add fields, never rename or remove, keep the previous version working.
5. Nothing during a service may depend on the cloud. If a change would add that dependency, stop and flag it.
6. Keep secrets (stream keys, API keys) out of the repo and out of logs.

## Stage 1A: build first, harden later
- One or two happy-path tests per feature. No new chaos tests, timing thresholds or kill loops.
- A slow, nightly or flaky failure: one line in `docs/HARDENING.md`, keep building.
- Order for new behavior: `packages/protocol` (with fixtures) → daemon → web app.
- PR gate: `.\dev.ps1 check` in `apps/daemon` (fmt, clippy, fast tier, npm typecheck and tests).
- Base branch for this stack: `web/local-db` (not `main`). The lead merges PRs once review and CI are done.

## Remaining items
- [x] Themes (fonts, colors, backgrounds) — PR #49 merged
- [x] Installable PWA that keeps working offline — PR #48 merged
- [x] Pre-flight check — PR #53 merged
- [x] Keyboard and clicker control on the computer itself — PR #50 merged
- [x] Blocks sleep during services — PR #51 merged (awake while streaming/recording,
  or for 3 h after the last operator change; `in_service()` is what a future updater must check)
- [x] Export in open formats — PR #52 merged
- [ ] Stream lyric layer uses the active theme (`apps/daemon/src/lyrics.rs`) — branch `daemon/lyrics-theme`
- [ ] Pictures move into the church computer's library, so plan and theme pictures show on every device — branch `web/library-media`
- [ ] Delete songs and plans from the UI — branch `web/delete-docs`
- [ ] Pre-flight: pending OS restart check — branch `daemon/preflight-restart`
- [ ] More public-domain hymns in `packages/songs` — branch `songs/more-hymns`
- Later in 1A (needs owner decisions first): no-audio and stream-down alerts (push + email), church account and Stripe, sync engine.

## Avoiding conflicts between parallel PRs
- `apps/web/src/routes/+page.svelte` is shared. Put new UI in a new component under `src/lib` and keep the
  `+page.svelte` change to a few lines that mount it.

## Already built (do not redo)
- Run sheet, pre-service loop, image and PDF slides
- Song editor and public-domain hymn library (`packages/songs`)
- Operator view with locked volunteer mode (`Operator.svelte`, `lock.ts`)
- Daemon serves the built web app on the channel port (`apps/daemon/src/web.rs`, `--web-dir`)
- SQLite library and plans: `apps/daemon/src/library.rs`, protocol `doc.*`. Documents are stored by kind
  (`song`, `plan`, `theme`, `setting`); the daemon keeps each body as given, so a new kind needs no daemon change.
  Web side: `apps/web/src/lib/library.ts`, `db.ts`.

## Directory map
- `apps/site` — live.jivvy.org (Astro, Cloudflare Pages)
- `apps/web` — app.jivvy.org, SvelteKit static SPA (`npm run dev:web`, port 5173); `src/lib` holds the screens and logic, `static/brand` the icons
- `apps/daemon` — Rust: `jivvy-watchdog` supervises `jivvy-engine`, `jivvy-outputs`, `jivvy-video`; lyric layer drawn natively in `src/lyrics.rs`
- `packages/protocol` — versioned command protocol (TypeScript, zero runtime deps)
- `packages/songs` — song format, paste parser, hymn library
- `packages/sim-daemon` — in-memory fake daemon; frozen during Stage 1A
- `docs/HARDENING.md` — deferred failures and hardening work
