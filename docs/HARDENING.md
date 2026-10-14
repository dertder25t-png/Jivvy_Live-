# Hardening list

Stage 1A builds first and hardens later (see `docs/BUILD_PLAN.md`, Stage 1). When a slow, nightly or flaky test fails, or a reliability check gets skipped to keep building, add one line here and keep going. Stage 1B starts by working through this list. Don't loosen a test or a target to make it pass; fix the cause in 1B.

Format: `- [ ] YYYY-MM-DD · test or area · symptom (link to the CI run if there is one)`

## Open

- [ ] 2026-10-14 · slow tier (`.\dev.ps1 slow`, video and streaming chaos) · not run when the stack ws-tls → preflight-summary landed; only the fast tier passed. Check the first nightly run.
- [ ] 2026-10-14 · `preflight::tests::the_stream_starting_stops_the_test` · on the Linux fast job the cancelled upload test took 3.79 s to stop (limit 3 s, cancel sent at 0.3 s); passes locally. Likely a blocking write that only checks the cancel flag between chunks (https://github.com/dertder25t-png/Jivvy_Live-/actions/runs/38599265377)

## Fixed
