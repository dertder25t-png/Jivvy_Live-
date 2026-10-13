# Secure command channel: hostname, certificate, pairing and the offline fallback

Oct 12, 2026. Covers the "Local secure command channel" item in `docs/BUILD_PLAN.md`: what's built, and what's left.

## What exists today

- The protocol over WebSocket (`--listen-ws`), browsers only from allowed origins.
- TLS on it (`--ws-cert`/`--ws-key`, PR #32): `wss://` only, no plain fallback, renewed certificates picked up without a restart.
- Pairing and device keys (PR #34), described in section 2.
- The TCP channel is the church computer itself (our own processes, `jivvy-ctl`, the booth screen). It is trusted, so it only takes connections from this computer (loopback), whatever address it listens on.

## Goals

1. Only devices the church approved can control a service. Being on the church Wi-Fi isn't enough.
2. Nothing about joining, pairing or controlling needs the internet on Sunday.
3. No control traffic, pairing code or key ever crosses the network in plain text, and we accept no path where a device could be handed fake app code.
4. A volunteer never sees a certificate warning, and never has to type an IP address.
5. Pairing never gets in the way of running the service (decided Oct 12: a small corner panel, never a pop-up, never on outputs or the stream, never takes focus).
6. Pairing and the code fallback are off until the church computer turns them on, and only it can (decided Oct 12).

## 1. The per-church hostname and certificate (normal path)

- Each church computer gets a hostname `<church-id>.d.jivvy.org`, where `<church-id>` is a random 10-character id (not the church's name, so hostnames reveal nothing).
- **The private key never leaves the church computer.** The daemon generates the key and a certificate request (CSR). It sends the CSR to a Jivvy cloud function, authenticated with the church's license, and the function gets the certificate from Let's Encrypt with a DNS-01 challenge on the `d.jivvy.org` zone (Cloudflare DNS API). The daemon writes `cert.pem` next to its key; the running engine picks it up (done).
- **Free forever:** Let's Encrypt costs nothing, so renewal costs nothing for every church, paid or not.
- **Renewal** starts when a third of the certificate's life is left and retries daily. Let's Encrypt is shortening lifetimes (toward 45 days), so nothing assumes 90. Renewal never runs during a service window, and an expiring certificate is a pre-flight red item days before it matters.
- **DNS:** `<church-id>.d.jivvy.org` points at the church computer's local address. The daemon tells the cloud when that address changes (checked at start and when the network changes, never mid-service).
- **Rate limits:** Let's Encrypt allows 50 certificates per registered domain per week. Thousands of churches under `jivvy.org` would hit that. **Before launch, `d.jivvy.org` goes on the Public Suffix List** (as Plex did with `plex.direct`), so each church's hostname counts on its own. The PSL review takes weeks, so file early; until then, alpha volumes fit the limit.
- **Routers that block local answers** ("DNS rebinding protection") stop the hostname working even with the internet up. Pre-flight detects it and says so plainly; those churches use the offline path (section 3).

## 2. Pairing a device (built, PR #34)

- **Device key:** the web app (from `app.jivvy.org` over https) creates a P-256 key with WebCrypto, not extractable, kept in IndexedDB. The daemon only ever stores public keys, in `devices.json`, saved durably; an unreadable file trusts nobody.
- **Opening pairing:** only the church computer can (`pair.open`), for 2 minutes. Requests at any other time are refused, so nobody on the Wi-Fi can fill the corner panel during a service. At most 5 requests wait.
- **Request and code:** the device sends `pair.request` with its key and a name. Both the phone and the church computer show the same random 6-digit code. Someone at the church computer checks the codes match and approves (`pair.approve`) or denies.
- **Every connection:** `auth.hello` returns a one-time challenge; the device signs `jivvy-live auth v1 / daemon id / challenge` and sends it with `auth.prove`. One try per challenge. The daemon id is made once per church computer, so a proof made for one church computer is useless on another, and TLS with the church's certificate stops anyone relaying it.
- **What a paired device may do:** run the service (slides, black, stream and recording on and off, state). Pairing, the device list and the video process's reports belong to the church computer alone.
- **Removal:** `device.list` and `device.remove` (church computer only). Removal ends the device's open connections at once.
- **Tests:** the sim and the real daemon both cover the full flow, wrong keys, signatures for another daemon, replays, the window closing, denial, the request limit, removal, crashes and restarts. The shared scenarios run over `ws` and `wss` with a device paired for real that proves its key on every reconnect through engine crashes; an unpaired device is refused everything but pairing and proving.

## 3. The offline fallback (QR code / short code)

Used when the hostname can't be reached: internet down, DNS blocked, or rebinding protection.

**Decided (Oct 12): the fallback never serves the web app over plain HTTP, and we don't accept that risk.** A plain-HTTP page from a local address could be swapped by someone spoofing the church Wi-Fi during an outage, and no in-page crypto can protect a page that is itself fake. So:

- The app's code only ever comes from `app.jivvy.org` over https, installed on the device (an installable web app) while it's online.
- Offline, the installed app reaches the church computer's local address directly over an encrypted channel a secure page is allowed to open (a WebRTC data channel, DTLS-encrypted). The QR code (or short code) carries the local address and the daemon's key fingerprint; the app refuses any other fingerprint, so a spoofed device on the network gets nowhere.
- Then the device proves its paired key exactly as it does online (section 2). An unpaired device can pair offline only with the code shown on the church computer and approval there, through the same genuine app.
- A device that has never opened the app while online can't join offline. It's told plainly: "Open Jivvy once with internet on this phone."
- **Off by default**, a setting on the church computer, hidden in volunteer mode, Tech Lead or above once roles exist.
- **Browser check first:** before building it, confirm Chrome, Safari and Firefox allow a WebRTC data channel from an installed https app to a local address with the internet down. A browser that doesn't gets no offline phone control, never a weaker path. Keyboard, clicker and Stream Deck on the church computer always work offline.

## 4. Still to test, as each piece lands

- Certificate issuance: the key never leaves the computer; renewal never during a service window; pre-flight warns before expiry.
- Fallback: off by default; codes expire and are single-use; the app refuses a wrong fingerprint; nothing in plain text on the wire (a test captures the bytes and checks that neither the code nor any command appears).
- Never: a code or key in a log, a URL or the stream.

## Order of work (one PR each)

1. ~~TLS on the WebSocket channel~~ (PR #32).
2. ~~Pairing and device keys: protocol, simulated daemon, real daemon, `jivvy-ctl`~~ (PR #34).
3. Certificate issuance: CSR in the daemon, cloud function, DNS record, renewal schedule, pre-flight item. File the PSL request.
4. The offline fallback: browser check, then the daemon's WebRTC endpoint, the setting, the corner panel and the QR code.
