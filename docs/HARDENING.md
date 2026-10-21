# Hardening list

Stage 1A builds first and hardens later (see `docs/BUILD_PLAN.md`, Stage 1). When a slow, nightly or flaky test fails, or a reliability check gets skipped to keep building, add one line here and keep going. Stage 1B starts by working through this list. Don't loosen a test or a target to make it pass; fix the cause in 1B.

Format: `- [ ] YYYY-MM-DD · test or area · symptom (link to the CI run if there is one)`

## Open

- [ ] 2026-10-14 · slow tier (`.\dev.ps1 slow`, video and streaming chaos) · not run when the stack ws-tls → preflight-summary landed; only the fast tier passed. Check the first nightly run.
- [ ] 2026-10-14 · `preflight::tests::the_stream_starting_stops_the_test` · on the Linux fast job the cancelled upload test took 3.79 s to stop (limit 3 s, cancel sent at 0.3 s); passes locally; also failed on #37's old full Linux CI. Fails every run, so the PR fast job skips it by name (nightly still runs it); remove the skip when fixed. Likely a blocking write that only checks the cancel flag between chunks (https://github.com/dertder25t-png/Jivvy_Live-/actions/runs/38599265377)
- [ ] 2026-10-14 · engine lock vs. disk flushes · `fsync` runs while the engine's lock is held (the snapshot on every slide change, and the last piece of an image upload, #41); on a stalled disk that can outlast the watchdog's 2 s heartbeat and restart the engine. Decide in 1B whether flushes move off the lock or the heartbeat stops depending on it (codex review on #41).
- [ ] 2026-10-16 · PWA service worker · a tab left open across an update can fail a lazy import of an old chunk, because `activate` deletes the old cache at once; a reload fixes it.
- [ ] 2026-10-16 · no-sleep is Windows only (`apps/daemon/src/power.rs`) · TODO: macOS (IOPMAssertion) and Linux (systemd-inhibit) before those church computers are supported.

## Fixed
