# Jivvy Live

See `docs/BUILD_PLAN.md` for the plan and `CLAUDE.md` for working rules.

```
npm install
npm test
npm run dev:site     # http://localhost:4321
```

## Deploying the site (Cloudflare Pages)
- Build command: `npm run build`  ·  Output directory: `apps/site/dist`  ·  Node 22
- Waitlist storage (D1): see comments in `apps/site/wrangler.toml`. Until the `DB` binding exists the form returns a friendly "not open yet" message instead of losing signups silently.
- Booking: set the build variable `PUBLIC_CALENDLY_URL` (an `https://calendly.com/...` link) to show the "Book a call" button. Without it the section falls back to the contact email.
