# Jivvy Live web app (app.jivvy.org)

SvelteKit, built as a static single-page app (`adapter-static`): Cloudflare Pages serves it, and the daemon will serve the same files on the church's network with no server code. It talks to the daemon directly over its WebSocket channel using `packages/protocol`; nothing on this screen needs the cloud.

Today: the run sheet screen. Build the service (songs, scripture, countdowns, text, a pre-service loop), send it to the screens, then Next/Back/Black or click any slide. Arrow keys, Page Up/Down and Space move slides; B goes black.

## Run it against the real daemon

1. Build the daemon (`apps/daemon`: `.\dev.ps1 build`), then start it with the WebSocket channel open to the dev page:
   ```powershell
   .\target\release\jivvy-watchdog.exe --test-sources --listen-ws 127.0.0.1:47801 --ws-origin http://localhost:5173
   ```
2. `npm run dev:web` (repo root) and open http://localhost:5173. Connect to `ws://127.0.0.1:47801`.
3. The first time, pair this browser: `.\dev.ps1 ctl pair open` on the church computer, click **Ask to pair**, check the code matches, then `.\dev.ps1 ctl pair approve <request id>` (the id is shown under the code). The browser's key stays in IndexedDB, so later visits connect straight away.

`npm test` (slide building), `npm run typecheck` (svelte-check), `npm run build` (writes `build/`).
