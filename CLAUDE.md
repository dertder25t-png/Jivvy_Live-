# Jivvy Live

Church live-production software. Source of truth: `docs/BUILD_PLAN.md`. Brand: `jivvy-brand-guide.md`.

## Rules for every session
1. Read the plan; pick the next unchecked item in the current stage. Do not start later stages early.
2. One item per branch and PR, small enough to review in one sitting.
3. Every change ships with tests; daemon changes also pass the chaos tests in the plan's reliability table.
4. Never break the command protocol: add fields, never rename/remove, keep the previous version working.
5. Nothing during a service may depend on the cloud. If a change adds that dependency, stop and flag it.
6. Keep secrets (stream keys, API keys) out of the repo and out of logs.

## Layout
- `apps/site` — live.jivvy.org (Astro, Cloudflare Pages + Pages Functions)
- `apps/web` — app.jivvy.org (not started; Stage 1)
- `apps/daemon` — church-computer app (Rust): `jivvy-watchdog` supervises `jivvy-engine`; chaos tests in `tests/chaos.rs`
- `packages/protocol` — versioned command protocol (TypeScript, zero runtime deps)
- `packages/sim-daemon` — in-memory fake daemon (demo + chaos tests); must honor the same reliability contract as the real one

## Commands
`npm install`, `npm test`, `npm run typecheck`, `npm run build`, `npm run dev:site`
Daemon: `cargo test --release` in `apps/daemon` (includes chaos tests, ~3 min); `cargo fmt` and `cargo clippy` must be clean

## Brand
Navy #10224F, Paper #F4F1EA, Jivvy blue #2E6BFF, On-air orange #FF7A1A (small accents only, never large areas).
