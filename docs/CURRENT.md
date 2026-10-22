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
- Base branch: `main`. The lead merges PRs once review and CI are done.

## Remaining items (round 4)
- [~] Sync engine — #65 merged (outbox, per-field merge, conflicts, status, against a fake). Supabase transport + site token exchange: PR open (daemon/sync-supabase). Next: owner setup (Supabase project, Pages secrets, D1 migration 0004), then a device list/remove on /account.
- Later: email alerts via the church account; Stripe go-live (owner).
- Merged so far: #48–#64 (see BUILD_PLAN.md).

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
