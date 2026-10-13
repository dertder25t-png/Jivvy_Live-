# Secure command channel: hostname, certificate, pairing and the offline fallback

Draft for review, Oct 12, 2026. Covers what is left of the "Local secure command channel" item in `docs/BUILD_PLAN.md`. None of the pairing or fallback crypto ships until this is reviewed (the plan's rule).

## What exists today

- The protocol over WebSocket (`--listen-ws`), browsers only from allowed origins.
- TLS on it (`--ws-cert`/`--ws-key`, PR #32): `wss://` only, no plain fallback, renewed certificates picked up without a restart.
- The TCP channel on loopback, used by our own processes (outputs, video, `jivvy-ctl`). It stays loopback-only and unauthenticated: anything that can reach it already runs on the church computer.

## Goals

1. Only devices the church approved can control a service. Being on the church Wi-Fi isn't enough.
2. Nothing about joining, pairing or controlling needs the internet on Sunday.
3. No control traffic, pairing code or key ever crosses the network in plain text.
4. A volunteer never sees a certificate warning, and never has to type an IP address.
5. Pairing never gets in the way of running the service (decided Oct 12: a small corner panel, never a pop-up, never on outputs or the stream, never takes focus).
6. The code fallback is off by default and only usable from the church computer (decided Oct 12).

## 1. The per-church hostname and certificate (normal path)

- Each church computer gets a hostname `<church-id>.d.jivvy.org`, where `<church-id>` is a random 10-character id (not the church's name, so hostnames reveal nothing).
- **The private key never leaves the church computer.** The daemon generates the key and a certificate request (CSR). It sends the CSR to a Jivvy cloud function, authenticated with the church's license, and the function gets the certificate from Let's Encrypt with a DNS-01 challenge on the `d.jivvy.org` zone (Cloudflare DNS API). The daemon writes `cert.pem` next to its key; the running engine picks it up (done).
- **Free forever:** Let's Encrypt costs nothing, so renewal costs nothing for every church, paid or not.
- **Renewal** starts when a third of the certificate's life is left and retries daily. Let's Encrypt is shortening lifetimes (toward 45 days), so nothing assumes 90. Renewal never runs during a service window, and an expired certificate still being served is a pre-flight red item days before it matters.
- **DNS:** `<church-id>.d.jivvy.org` points at the church computer's local address. The daemon tells the cloud when that address changes (checked at start and when the network changes, never mid-service).
- **Rate limits:** Let's Encrypt allows 50 certificates per registered domain per week. Thousands of churches under `jivvy.org` would hit that. **Before launch, `d.jivvy.org` goes on the Public Suffix List** (as Plex did with `plex.direct`), so each church's hostname counts on its own. The PSL review takes weeks, so file early; until then, alpha volumes fit the limit.

**Open question A:** some routers block DNS answers that point at local addresses ("DNS rebinding protection"). Those churches fall back to the QR path even with the internet up. We could detect it in pre-flight and say so plainly. Alternative: encode the IP in the name (`192-168-1-20.<id>.d.jivvy.org`, wildcard certificate), so there are no DNS updates. But that still needs a public DNS answer and is blocked the same way, and it needs our own DNS server. **Recommendation:** use plain A records and detect rebinding protection in pre-flight.

## 2. Pairing a device (online, the normal path)

Origins alone don't stop someone on the church Wi-Fi with a script. Every device that isn't on the church computer must be paired.

- **Device key:** on first use, the web app (served over https from `app.jivvy.org`) creates a P-256 signing key with WebCrypto, marked non-extractable, and keeps it in IndexedDB. The device's identity is that key; there are no passwords and nothing to steal from the daemon.
- **Pairing request:** the device connects over `wss://` and asks to pair, sending its public key and a name ("Sarah's iPhone"). The daemon answers with a 6-digit number derived from both public keys and a fresh nonce. The number shows on the phone and in the church computer's corner panel.
- **Approval on the church computer only:** someone at the church computer checks that the numbers match and clicks Approve. That approval is the only way in. Paired phones can't approve or invite anyone (decided Oct 12). Once roles exist, approving needs Tech Lead or above.
- **Every later connection:** the daemon sends a random challenge; the device signs it, together with the TLS channel binding. The daemon checks the signature against its list of paired keys. A device with no key, or a removed key, gets nothing but the pairing request.
- **Removal:** the church computer lists paired devices (name, when paired, last seen) and can remove any of them. Removal closes that device's open connections at once.
- **Protocol:** new commands (`auth.hello`, `auth.prove`, `pair.request`, `pair.status`) and new events, added beside the current ones. Nothing is renamed or removed. They go in `packages/protocol` and the shared scenarios first, then the simulated daemon, then the real one (the repo's order).
- **Storage:** paired devices live in the daemon's data folder (moving into the local SQLite database when that lands), written atomically like the snapshot.

## 3. The offline fallback (QR code / short code)

Used when the hostname can't be reached: internet down, DNS blocked, or rebinding protection.

- **Off by default.** It's a setting on the church computer, hidden in volunteer mode, and needs Tech Lead or above once roles exist (decided Oct 12).
- When it's on and someone asks for a code, the corner panel shows a QR code and a short code. The QR code holds the computer's local address, a one-time secret and the daemon's key fingerprint. The short code is a typeable form of the address plus a 6-digit secret.
- The QR code opens the web app that the daemon serves on the local network (the plan's "daemon serves the full web app" item). That page talks to the daemon over an encrypted, authenticated channel in the page's own code: a password-authenticated key exchange (CPace or SPAKE2) keyed by the code's secret, then an AEAD-encrypted session. Browsers withhold WebCrypto from non-https pages, so the crypto is a small audited library bundled into the page (e.g. `@noble/curves` and `@noble/ciphers`).
- Codes are single-use, expire after 2 minutes, and stop working after 3 wrong tries. That rules out guessing a 6-digit secret.
- A device paired through the fallback is still approved on the church computer, like any other pairing.

**Open question B (the important one): the page itself arrives over plain HTTP.** The key exchange protects the channel only if the page's code is genuine. During an internet outage, someone on the church Wi-Fi who can spoof the network (ARP or DNS spoofing) could serve a fake page to the person who scans the QR code. That page would capture the code and relay the pairing as a man in the middle. No browser can check a top-level page against a hash from the QR code. This is a real limit of the "plain local address" approach, not a detail. Options:

1. **Accept it, with least privilege (recommended for alpha).** Devices paired through the fallback get operator controls only: next, back, black screen, start and stop the stream and recording. They never get settings, stream keys, pairing or device removal. Their access ends when the internet path works again, and they must pair normally the next time. Approval on the church computer still gates every pairing, and the corner panel shows the device's name and type. The attack needs someone actively spoofing the church network during an outage, at the moment a code is scanned.
2. **Keep the page on the https origin.** Devices paired while online already have the app cached from `app.jivvy.org`. Chrome's Local Network Access lets a secure page call a local address with the user's permission. If that covers WebSockets from cached pages, those devices never need the plain-HTTP page. Safari and iOS don't support this, so this is an addition, not a replacement. **To verify before relying on it.**
3. **No fallback page at all; keyboard, clicker and Stream Deck on the church computer are the offline answer.** This is the safest, but it drops phone control when the internet is down.

**Recommendation:** option 1 now. Add option 2 where browsers support it, so most paired phones never touch the plain-HTTP page. Revisit before launch with an outside security review.

## 4. What we test

- TLS (PR #32): `wss` only, renewal without restart, open connections stay, bad pairs keep the last good one, keys never in logs).
- Pairing: an unpaired device gets nothing but the pairing request; a removed device is cut off at once; a stolen challenge response can't be replayed on another connection (channel binding); approval only from the church computer.
- Fallback: off by default; codes expire, are single-use and lock after 3 tries; nothing in plain text on the wire (a test captures the bytes and checks that neither the code nor any command appears); fallback devices can't reach settings or keys.
- Chaos: the engine is killed mid-pairing and mid-session; paired devices reconnect and re-prove within the 3-second target; the shared scenarios pass over `wss` with authentication.
- Never: a code or key in a log, a URL or the stream.

## Order of work (one PR each)

1. ~~TLS on the WebSocket channel~~ (PR #32).
2. Pairing commands and events in `packages/protocol` plus scenarios; the simulated daemon.
3. Pairing and challenge auth in the real daemon; paired-device list and removal.
4. Certificate issuance: CSR in the daemon, cloud function, DNS record, renewal schedule, pre-flight item. File the PSL request.
5. The fallback, after open question B is decided: the setting, the corner panel, the code, the key exchange, least-privilege sessions.
