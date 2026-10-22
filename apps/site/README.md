# live.jivvy.org

Astro on Cloudflare Pages, with Pages Functions in `functions/` (`npm run dev:site`, `npm test -w @jivvy/site`).

- `/` the public site: features, pricing, FAQ and the alpha waitlist (`/api/waitlist`, D1 table `waitlist`).
- `/api/upload-test`: the pre-flight upload test's endpoint (counts what arrives, stores nothing).
- `/account` church accounts and license keys (not linked from the site yet). A church signs in with a one-time code emailed to the address it typed, buys a license with Stripe Checkout, and copies the key into Jivvy Live on the church computer. The trial needs none of this: the church computer starts it by itself, with no account and no card. Nothing here is needed during a service, because the church computer checks its key offline.

## Account and checkout endpoints

| Endpoint | What it does |
|---|---|
| `POST /api/account/code {email}` | Emails a 6-digit code (10 minutes, 5 tries, at most one a minute) to that address only. Never to the waitlist. |
| `POST /api/account/verify {email, code}` | Signs in with a session cookie (30 days), and makes the church account on first sign-in. |
| `GET /api/account` | The signed-in church and its license keys. |
| `POST /api/account/logout` | Ends the sign-in. |
| `POST /api/checkout` | A Stripe Checkout Session for one church license (test mode only: anything but an `sk_test_` key is refused). |
| `POST /api/stripe-webhook` | Checks `Stripe-Signature`, and on `checkout.session.completed` (paid, `livemode: false`) issues the key: Ed25519-signed `{church, plan: "church", updatesUntil: +3 years, plusUntil: +1 year, issued}` (`packages/protocol/src/license.ts`). Retried deliveries issue nothing new (one license per checkout). |

Codes and session tokens are stored only as SHA-256 hashes.

## What the owner sets up (nothing is created by the code)

None of this is in the repo. Each one is optional at runtime: when it's missing, that endpoint answers 503 and nothing else breaks.

**D1** (binding `DB`, already in `wrangler.toml`): apply the new migration to both databases before deploying:

```
npx wrangler d1 migrations apply jivvy-live --remote
npx wrangler d1 migrations apply jivvy-live-preview --remote --env preview
```

**Email (Cloudflare Email Sending)**, for sign-in codes only:
- Onboard the sending domain to Email Sending, then add a `send_email` binding named `EMAIL` to the Pages project (or in `wrangler.toml`: `[[send_email]]` with `name = "EMAIL"`).
- `EMAIL_FROM` (variable): the sender address, on that domain.

**Secrets** (Pages, for production and preview; never committed, never logged):

| Name | What |
|---|---|
| `LICENSE_PRIVATE_KEY` | The production Ed25519 private key, PKCS#8, base64url. Make the pair once (command in `apps/daemon/README.md`, "License and trial"); the public half goes into release daemon builds as `JIVVY_LICENSE_PUBLIC_KEY`. Never use the fixture's published test key here. |
| `LICENSE_TEST_PRIVATE_KEY` | The published test signing key (`testOnlyPrivateKeyPkcs8` in `packages/protocol/fixtures/license.json`). Test-mode purchases are signed with this, never with `LICENSE_PRIVATE_KEY`; only debug daemons accept those keys. The webhook issues nothing without it. |
| `STRIPE_SECRET_KEY` | The Stripe **test** secret or restricted key (`sk_test_…` or `rk_test_…`). Live keys are refused until the code is changed on purpose. |
| `STRIPE_WEBHOOK_SECRET` | The signing secret (`whsec_…`) of a webhook endpoint at `https://live.jivvy.org/api/stripe-webhook` listening for `checkout.session.completed` and `checkout.session.async_payment_succeeded`. |

**Variables:** `STRIPE_PRICE_ID` (a test-mode one-time price of exactly $200 USD; the webhook issues a key only for a `payment` session of 20000 usd) and `SITE_URL` (e.g. `https://live.jivvy.org`, used for Stripe's return links).

Try it with Stripe's test card `4242 4242 4242 4242`. No real charge can happen with a test key.
