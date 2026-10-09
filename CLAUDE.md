# Jivvy Live

Church live-production software. Source of truth: `docs/BUILD_PLAN.md`. Brand: `jivvy-brand-guide.md`.

## Rules for every session
1. Read the plan; pick the next unchecked item in the current stage. Do not start later stages early.
2. One item per branch and PR, small enough to review in one sitting.
3. Every change ships with tests; daemon changes also pass the chaos tests in the plan's reliability table.
4. Never break the command protocol: add fields, never rename/remove, keep the previous version working.
5. Nothing during a service may depend on the cloud. If a change adds that dependency, stop and flag it.
6. Keep secrets (stream keys, API keys) out of the repo and out of logs.
7. No church outreach, calls, pilots or surveys until Stage 1 is done; the waitlist gets no email until alpha testing opens (decided Oct 6, 2026).

## Layout
- `apps/site` — live.jivvy.org (Astro, Cloudflare Pages + Pages Functions)
- `apps/web` — app.jivvy.org (not started; Stage 1 alpha)
- `apps/daemon` — church-computer app (Rust): `jivvy-watchdog` supervises `jivvy-engine`, `jivvy-outputs` (fullscreen output windows) and `jivvy-video` (camera/audio, `--features video`); chaos tests in `tests/chaos.rs`
- `packages/protocol` — versioned command protocol (TypeScript, zero runtime deps)
- `packages/sim-daemon` — in-memory fake daemon (demo + chaos tests); must honor the same reliability contract as the real one

## Commands
`npm install`, `npm test`, `npm run typecheck`, `npm run build`, `npm run dev:site`
Daemon (in `apps/daemon`): `.\dev.ps1 check` (fmt, clippy, fast tier, npm typecheck and tests; ~30 s, no GStreamer); `.\dev.ps1 test-full` (everything: video and streaming tests, the full 100 kills, npm; needs GStreamer, and MediaMTX in `apps/daemon/.tools/`; see the daemon README); `.\dev.ps1 fast` / `slow` run one tier; `.\dev.ps1 ctl next` drives a running daemon; `cargo fmt` and `cargo clippy` must be clean

## Workflow
- While iterating: the fast tier (`.\dev.ps1 fast`). It never covers video or streaming, and says so.
- Before every push: `.\dev.ps1 check`.
- Once per PR: `.\dev.ps1 test-full`, and `.\dev.ps1 slow` before any PR touching video, stream, bandwidth, HLS or tiers code. CI runs everything on Windows and Linux at full strength either way.
- New behavior goes in this order: `packages/protocol` (command and fixtures), then `packages/sim-daemon`, then the real daemon. Describe it in `packages/protocol/fixtures/scenarios.json` first; both daemons must pass it.
- Never shorten a chaos test's outage or "nothing happens" waits, loosen a reliability target from the plan (back on the same slide within 3 s, stream recovers by itself from a blip under 30 s, no-audio or stream-down alert within 15 s), or loosen a chaos test's threshold (program at 25 fps or more, YouTube HLS caught up within 15 s of the connection returning) to make a test pass; report the margin instead.
- Run the daemon without a camera or microphone with `--test-sources`.

## Brand
Navy #10224F, Paper #F4F1EA, Jivvy blue #2E6BFF, On-air orange #FF7A1A (small accents only, never large areas).
