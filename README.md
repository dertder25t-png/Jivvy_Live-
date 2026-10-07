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
- Waitlist schema changes go in `apps/site/migrations/`. Apply them to both databases (commands in `wrangler.toml`) before merging code that needs them.
- To export the waitlist (e.g. to email alpha testers): `npx wrangler d1 execute jivvy-live --remote --command "SELECT email, church, alpha_tester FROM waitlist"` from `apps/site`.
