# Jivvy Live

Church live-production software. Source of truth: `docs/BUILD_PLAN.md`. Brand: `jivvy-brand-guide.md`.

## Rules for every session
1. Read the plan; pick the next unchecked item in the current stage. Do not start later stages early.
2. One item per branch and PR, small enough to review in one sitting.
3. Build first, harden later (decided Oct 14, 2026). In Stage 1A each feature ships with one or two happy-path tests that prove it works. No new chaos tests, timing thresholds or kill loops until Stage 1B (Harden).
4. Never break the command protocol: add fields, never rename/remove, keep the previous version working.
5. Nothing during a service may depend on the cloud. If a change adds that dependency, stop and flag it.
6. Keep secrets (stream keys, API keys) out of the repo and out of logs.
7. No church outreach, calls, pilots or surveys until Stage 1 is done; the waitlist gets no email until alpha testing opens (decided Oct 6, 2026).

## Layout
- `apps/site` — live.jivvy.org (Astro, Cloudflare Pages + Pages Functions)
- `apps/web` — app.jivvy.org (not started; next up in Stage 1A)
- `apps/daemon` — church-computer app (Rust): `jivvy-watchdog` supervises `jivvy-engine`, `jivvy-outputs` (fullscreen output windows) and `jivvy-video` (camera/audio, `--features video`); chaos tests in `tests/chaos.rs`
- `packages/protocol` — versioned command protocol (TypeScript, zero runtime deps)
- `packages/sim-daemon` — in-memory fake daemon for the Stage 2 demo; frozen during Stage 1A, catches up with the real daemon before the demo

## Commands
`npm install`, `npm test`, `npm run typecheck`, `npm run build`, `npm run dev:site`
Daemon (in `apps/daemon`): `.\dev.ps1 check` (fmt, clippy, fast tier, npm typecheck and tests; ~30 s, no GStreamer); `.\dev.ps1 test-full` (everything: video and streaming tests, the full 100 kills, npm; needs GStreamer, and MediaMTX in `apps/daemon/.tools/`; see the daemon README); `.\dev.ps1 fast` / `slow` run one tier; `.\dev.ps1 ctl next` drives a running daemon; `cargo fmt` and `cargo clippy` must be clean

## Workflow
- While iterating: the fast tier (`.\dev.ps1 fast`).
- Before every push: `.\dev.ps1 check`. That is the whole PR gate; PR CI runs the same on Linux.
- `.\dev.ps1 test-full` and `slow` are not required per PR. The full Windows + Linux video/streaming/chaos run happens nightly in CI and does not block merges.
- A slow, nightly or flaky test fails: add one line to `docs/HARDENING.md` and keep building. Fix it now only if it is a real bug in the feature you are building. Never spend a session chasing test timing in Stage 1A.
- New behavior goes in this order: `packages/protocol` (command and fixtures; never skip, the protocol must not break), then the real daemon, then the web app. `packages/sim-daemon` and `scenarios.json` are optional in Stage 1A.
- In Stage 1B (Harden), never shorten a chaos test's outage or "nothing happens" waits, loosen a reliability target from the plan (back on the same slide within 3 s, stream recovers by itself from a blip under 30 s, no-audio or stream-down alert within 15 s), or loosen a chaos test's threshold (program at 25 fps or more, YouTube HLS caught up within 15 s of the connection returning) to make a test pass; report the margin instead. In Stage 1A, log it in `docs/HARDENING.md` instead of touching the test.
- Run the daemon without a camera or microphone with `--test-sources`.

## Brand
Navy #10224F, Paper #F4F1EA, Jivvy blue #2E6BFF, On-air orange #FF7A1A (small accents only, never large areas).
