# Jivvy Live Build Plan

Oct 4, 2026 · @caleb

## Goals and principles

The product wins on one thing: a service never stops because of Jivvy Live. Every design choice below serves that.

1. **Nothing during a service depends on the cloud.** Video, slides, recording and booth control all run on the church computer and local Wi-Fi. The cloud handles planning, sync and remote access, so a cloud outage never touches a live service.
2. **Video never touches our servers by default.** Streams go straight from the church to YouTube, Facebook and others. This keeps hosting at roughly $50–100/month total. The one exception is the optional Pro relay (see Streaming design), which runs on Cloudflare Stream, not servers we operate, and always falls back to streaming direct.
3. **The UI is separate from the video engine.** A frozen phone, tab or editor can't stop the stream.
4. **Everything recovers by itself.** Crashes restart in seconds and resume on the same slide; dropped streams reconnect; recordings survive crashes.
5. **Volunteer-proof by default.** Locked-down views, plain-English errors, and a pre-flight check before every service.
6. **One codebase for the UI.** The same web app runs in a browser, as an installed app on phones, and inside the output windows.

"100% uptime" is the goal for service time specifically. We measure it as: zero services where slides, stream or recording stopped because of our software.

## Architecture

Three parts: a cloud for planning, a web app on every device, and a daemon on the church computer that does all the live work.

&#91;embedded content: system architecture · cloud, devices, daemon, outputs\]

- **Daemon:** a watchdog process supervises the video engine, which drives the screens, composites lyrics over camera, encodes once with the hardware chip, and sends that to the stream, the recording and the lobby feed.
- **Command channel:** devices send small versioned messages ("next slide", "go live") straight to the daemon over local Wi-Fi. The daemon accepts the current and previous protocol version so a slightly older UI still works.
- **Offline behavior:** the URL never changes. If the internet drops, the cached app keeps talking to the daemon locally, cloud features pause behind a banner, and edits sync when the connection returns.
- **Emergency remote:** the daemon also serves one bare page (Next, Back, Black screen, Stop stream) for a device that never loaded the app.

## Tech stack

Use proven, mostly free building blocks and write only the parts that make Jivvy Live different.

| Layer | Choice | Why |
| --- | --- | --- |
| Desktop app shell | Tauri or Electron (decide in Stage 1, week 1) | Tray icon, auto-start, updater, output windows on each monitor. See the note below. |
| Video engine | Rust + GStreamer | Live pipelines with hardware encoders, bitrate changes on the fly, one encode sent to both stream and file |
| Hardware encoding | Intel Quick Sync, NVIDIA NVENC, AMD AMF, Apple VideoToolbox; x264 fallback | Low CPU on cheap laptops, software fallback when no chip is usable |
| Streaming out | RTMP/RTMPS with auto-reconnect; SRT where a platform accepts it | What YouTube and Facebook take, no server in between |
| Cloud relay (Pro only) | Cloudflare Stream live inputs with simulcast outputs | Upload once, fan out to up to 50 platforms; about $1 per 1,000 minutes sent, nothing for us to run |
| Recording | Matroska or fragmented MP4, converted to MP4 after service | A crash never corrupts the file |
| Lobby TVs | Local HLS from the daemon | Live video over Wi-Fi with no internet use |
| Web app | SvelteKit or Next.js as an installable PWA, hosted on Cloudflare Pages at app.jivvy.org | One UI for browser, phones and output windows; works offline from cache |
| Local control | Secure WebSocket on a per-church hostname under d.jivvy.org with a real certificate (the Plex approach) | Browsers block insecure local connections from https pages |
| Cloud | Supabase: Postgres, Auth, Realtime, Edge Functions | Plans, songs, roles, share links, sync, heartbeats |
| Media files | Cloudflare R2 | Cheap storage, free downloads for slides and videos |
| Alerts | Web push and email; SMS optional later | Free channels first |
| Crash reports | Sentry | See daemon crashes before churches report them |
| Payments and licenses | Stripe + license keys tied to the church account | One-time $200 and Plus renewals |

**Tauri vs. Electron — test before committing.** The hardest piece is putting web-built lyrics onto the video. Electron has built-in offscreen rendering, so the same HTML that draws the projector screen can become the video's lyric layer. Tauri is much lighter but would need a second renderer for that. Build a one-week prototype of "web lyrics over camera, hardware-encoded, streamed to YouTube" in each and pick the one that holds 1080p30 on a $300 laptop.

## Domains and hosting

Everything lives under jivvy.org, so no new domain is needed; this project is hosted on Cloudflare, not Vercel.

| Address | What it is | Hosted on |
| --- | --- | --- |
| jivvy.org | Ministry brand home; links to Jivvy Live | Unchanged |
| live.jivvy.org | Jivvy Live website: features, pricing, FAQ, waitlist, Google Calendar booking | Cloudflare Pages (Astro) |
| live.jivvy.org/demo | Self-serve demo | Cloudflare Pages |
| app.jivvy.org | The web app churches sign in to | Cloudflare Pages |
| \*.d.jivvy.org | Per-church hostnames for booth computers, each with its own certificate | Cloudflare DNS |

- Manage jivvy.org's DNS in Cloudflare (free) so subdomains and per-device certificates can be automated through its API.
- Media files go in Cloudflare R2 in the same account.
- Supabase stays the database, sign-in and realtime layer.

## Reliability design

No software is literally 100% up, so we design so that every likely failure is survived automatically and measure against hard targets.

**Targets**

- Daemon crash → back on the same slide in under 3 seconds, recording uninterrupted (or resumed into the same file set).
- Internet blip under 30 seconds → stream recovers without anyone touching it.
- Zero corrupted recordings, ever.
- Cloud outage → zero effect on a live service.
- No-audio or stream-down → alert to the tech lead within 15 seconds.

**Failure modes**

| Failure | Mitigation | How we test it |
| --- | --- | --- |
| Video engine crashes | Separate watchdog process restarts it and restores state from a local snapshot written every second | Kill the process 100 times in a test run |
| UI or phone freezes | Video engine runs independently; any other device can take over | Freeze the browser mid-service |
| Internet drops | Encoder buffers and reconnects; local recording continues; control stays on local Wi-Fi | Pull the network cable during a live stream |
| Slow upload | Encoder lowers bitrate in steps, raises it back when stable | Throttle the connection to 1 Mbps |
| Wi-Fi router dies | Keyboard, clicker, Stream Deck and MIDI control directly on the computer; optional hotspot | Turn off the router mid-service |
| Laptop sleeps or Windows Update restarts | Daemon blocks sleep during services; pre-flight warns about pending OS restarts | Schedule an OS update before a test service |
| Our update breaks something | No installs during service windows; staged rollout; one-tap rollback to the previous version | Ship a deliberately broken build to the test ring |
| Disk fills up | Pre-flight checks free space; recorder warns at 10 GB left | Fill a test drive |
| Camera or audio device unplugged | Hold the last frame or a slate, alert, reconnect automatically when it returns | Unplug the capture card mid-stream |
| Platform rejects the stream (bad key, expired event) | Pre-flight verifies keys; platform status checked through their APIs | Use an expired stream key |
| Cloud (Supabase, Cloudflare) is down | Booth runs from cached UI and local data; changes sync later | Block cloud domains during a test service |
| Corrupted settings or library | Local backups of the last 5 good states; restore from cloud copy | Corrupt the settings file |

**Before every service:** the pre-flight check runs all of the above checks in about 30 seconds and lists any red items with a fix.

## Streaming design

Streaming runs entirely from the church computer, aiming for 80–90% of Resi's reliability with zero server cost and nothing for us to keep running on Sundays.

**Resilience**

- **YouTube:** segment upload through YouTube's HLS ingest. Every few seconds of video is saved to disk and uploaded with retries until YouTube confirms it, then the backlog catches up faster than real time after a drop.
- **Other platforms (Facebook and others):** RTMPS with auto-reconnect and stepped bitrate.
- **Two connections at once:** church internet plus a phone hotspot or USB cellular modem; each segment goes over whichever connection works.
- **Save the stream:** local recording always runs. After service, one tap uploads the flawless recording to the chosen platforms, replacing a live stream that had problems.

**Bandwidth manager**

- Measures upload speed continuously and spends about 70% of it, keeping the rest for catching up after drops.
- **Auto mode (default):** picks the main platform and qualities, and cuts the lowest-priority platform first when upload gets tight.
- **Advanced mode:** the church ranks its platforms (Facebook first if they want), sets a max quality per platform, and chooses what happens when bandwidth runs short: lower quality, audio only, or pause.
- Pre-flight shows what the connection supports, e.g. "YouTube 1080p + Facebook 720p."
- The tech lead gets an alert whenever a platform is lowered or paused.

**Cloud relay (Pro tier, Stage 3)**

For churches with weak upload or many platforms: the church computer sends one stream (SRT for lossy connections, or RTMPS) to a Cloudflare Stream live input, and Stream copies it to every platform.

- **Never a single point of failure.** If the relay is unreachable or a platform's health check fails, the daemon streams straight to the platforms as it would without Pro, with the bandwidth manager deciding what fits. The relay can only make a stream better, never stop it.
- **Pass-through only.** Every platform gets the same quality; no transcoding on our side.
- **Recording off** on the live input; the church computer already records locally, so Stream storage costs nothing.
- **Cost:** Stream bills $1 per 1,000 minutes sent to platforms; ingest is free. A 90-minute service to 3 platforms is about $0.27, so roughly $1–4 per church per month. Stream needs a paid subscription for the account (shared by all churches).
- **Stream keys** are stored as Stream outputs through the API and never logged.
- **Prove these in a 1–2 day spike first:** (1) RTMPS output to Facebook works; (2) what Stream does when a platform drops the connection mid-stream; (3) there's no published uptime guarantee, so measure fallback time with the relay blocked.
- **Build when** Stage 1 has shipped, pilot churches ask for it, and Sunday mornings are covered.

## Stage 0: Validate (2–4 weeks)

Prove churches want it before writing the daemon.

- [ ] Send the church overview doc to 10–15 church tech leads and collect answers
- [x] Website at live.jivvy.org (Astro on Cloudflare Pages) with features, pricing, FAQ, waitlist and Google Calendar booking
- [ ] Optional "founding church" preorder to test real willingness to pay
- [ ] Line up 3–5 pilot churches you can visit in person
- [ ] Tech spike: web lyrics composited over a camera feed, hardware-encoded, streamed to YouTube, on a $300 laptop

**Done when:** at least 10 churches say they'd switch, 3–5 commit to piloting, and the tech spike holds 1080p30 with CPU under 40%.

## Self-serve demo and booking

Anyone can try Jivvy Live in their browser with no install, no account and no involvement from the founder; questions go to a booking page on the founder's schedule.

**The demo (runs entirely in the browser, free static hosting)**

- [ ] Same web app as the real product, connected to a simulated church computer that runs in the browser. Design the command protocol so a fake daemon can stand in; it doubles as a testing tool.
- [ ] Demo church preloaded: a full Sunday run sheet with public-domain hymns, scripture, sermon slides, announcements and a countdown
- [ ] "Projector" opens in a second tab or on a TV; a QR code turns the visitor's phone into the remote
- [ ] Stream preview uses their webcam (or a sample video) with lyrics on top
- [ ] Failure buttons: "cut the internet," "crash the app," "slow upload" — the visitor watches it recover. This is the showpiece.
- [ ] Pre-flight check, volunteer mode and a sample share link to try
- [ ] Short guided tour (5 steps), skippable
- [ ] Resets every visit; optional email to save their setup and start the 30-day trial

**Questions without the founder on call**

- [ ] FAQ and 1–2 minute how-to videos covering the common questions
- [ ] "Got questions? Book a call" button using a Google Calendar booking page with only the time slots the founder opens
- [ ] A short booking form (church size, current software, biggest problem) so every call is focused
- [ ] Email contact form for people who'd rather not call
- [ ] Privacy-friendly analytics on which demo steps visitors use and where they leave

**When:** build the demo right after the Stage 0 tech spike. It doubles as the validation tool — send it with the church overview instead of just a document.

## Stage 1: MVP that survives a Sunday (10–14 weeks)

The smallest product a pilot church can run a whole service on, with every reliability feature built in from the start.

**Daemon (church computer)**

- [ ] Watchdog + engine as separate processes, state snapshot every second, auto-restart on the same slide
- [ ] Fullscreen output windows per monitor, custom resolutions (reuse the GameWall DisplayHost pattern)
- [ ] Camera/capture card and audio interface input, with live audio meter
- [ ] Lyric layer composited over camera, hardware encode, one encode sent to stream and recording
- [ ] Streaming per the Streaming design section: YouTube segment upload with retries, RTMPS for other platforms, bandwidth manager with Auto and Advanced modes, two-connection support
- [ ] Crash-safe local recording
- [ ] Local secure command channel (per-church hostname + certificate); certificate renewal stays free forever for every church
- [ ] Daemon serves the full web app on the local network, so a church can run with no cloud account at all
- [ ] Keyboard and clicker control on the computer itself
- [ ] Blocks sleep during services; no updates during service windows; signed auto-updates with rollback

**Web app**

- [ ] Run sheet: songs, scripture, images, PDF slides, countdown, pre-service loop
- [ ] Song editor with arrangement blocks; paste-to-slides with section detection; public-domain hymn library
- [ ] Themes (fonts, colors, backgrounds)
- [ ] Operator view and locked volunteer mode
- [ ] Installable PWA that keeps working offline
- [ ] Pre-flight check
- [ ] No-audio and stream-down alerts (web push + email)
- [ ] Church account, Stripe checkout, license key, 30-day trial
- [ ] Feature gating by release date: every gated feature has a `releasedAt`; a license unlocks local features released before its updates window ends and cloud features while Plus is active. Checked offline, never during a service window, never shown to volunteers or on screen
- [ ] Export of songs, run sheets, themes and settings in open formats, available on every license

**Done when:** 3 pilot churches run 4 Sundays each with zero service-stopping failures, and a first-time volunteer runs a service after a 5-minute walkthrough.

## Stage 2: Team features (6–8 weeks)

Add what makes Jivvy Live the place the whole team coordinates, not just the booth. Items tagged **(Plus)** or **(Pro)** use the cloud and always need that subscription; untagged items run on the church computer and follow the 3-year updates rule. See "Licensing and updates."

- [ ] Collaborator links (no account): worship leader arrangements, guest speaker uploads, announcement submissions **(Plus)**
- [ ] Link builder for tech leads: choose what the link shows, add instructions or a voice note, preview as receiver, save templates **(Plus)**
- [ ] Sermon archive and podcast: audio trimmed from the local recording, published to a sermon page and podcast feed (audio on R2; video links to the platform's replay, never hosted by us) **(Plus)**
- [ ] "Watch live" page and website embed that switches on when the church goes live, using the platform's player **(Plus)**
- [ ] Lock time with approval for late changes; band read-only view
- [ ] Guest files: PDF/images rendered in browser, PowerPoint converted on the church computer, auto-delete after 30 days
- [ ] Pastor phone remote
- [ ] Stage display: next line, clock, countdown, producer messages
- [ ] Local HLS feed for lobby and nursery TVs
- [ ] Importers: ProPresenter, EasyWorship, OpenLP/OpenLyrics, SongSelect files; CCLI usage report export

**Done when:** pilot churches use share links for at least half their services without the tech lead re-entering anything by hand.

## Stage 3: Roles, scheduling and integrations (6–8 weeks)

Make it safe for bigger teams and connect it to the rest of the church's software.

- [ ] Roles: Owner, Admin, Tech Lead, Operator, Contributor, Viewer; stream keys hidden below Tech Lead
- [ ] Admin PIN unlock at the booth; password + 2-step verification for remote admin
- [ ] Activity log with one-tap undo
- [ ] Simple scheduling: positions, assignments, accept/decline, swap requests, day-before reminders, calendar feed **(Plus)**
- [ ] Sign in with MinistryBase ID; MinistryBase events and calendar sync **(Plus)**
- [ ] Planning Center import (plans, songs, schedules) **(Plus)**
- [ ] Public REST API + webhooks (service started, slide changed, stream down) **(Plus)**
- [ ] Zapier integration **(Plus)**; Stream Deck plugin; MIDI/OSC and Bitfocus Companion control
- [ ] Practice mode, built-in help, "flag a problem" button with logs attached
- [ ] Post-service report email with viewers across all platforms combined **(Plus)**
- [ ] Cloud backup of library, themes and settings with restore to a new computer; off-site recording backup for 90 days **(Plus)**
- [ ] Remote booth view and health dashboard for the tech lead at home **(Plus)**
- [ ] Support chatbot answering from the help docs, handing off to email or a booked call when unsure; points to pre-flight fixes during service hours **(Plus)**
- [ ] Licensed Bible translations (NIV, ESV and others), only after a publisher quote fits the Plus budget **(Plus)**
- [ ] Relay spike: the three checks in Streaming design → Cloud relay
- [ ] Cloud relay via Cloudflare Stream with automatic fallback to direct streaming; passes the Internet-drops and Cloud-down chaos tests with the relay blocked **(Pro)**
- [ ] Pro direct support line for Sunday mornings (text or phone) **(Pro)**

**Done when:** a church with 15+ volunteers runs a month of services with schedules and roles, and at least one MinistryBase church uses the integration.

## Stage 4: Assist features (8–12 weeks)

The features that wow in demos. They all run on the church computer, so they add no server cost; each one only suggests, and a person stays in control.

- [ ] Lyric follow: listens to the vocal mic, matches the known lyrics, and pulses the Next button when it's time
- [ ] Beat countdown: tempo detection showing "next slide in 4… 3… 2…" plus last-line highlight
- [ ] Scripture auto-detect: the pastor says a reference and the verse is queued for one-tap display
- [ ] Live captions on screen and stream (captions are accessibility, so free on every license forever); translation
- [ ] Auto sermon clips (30–60 seconds) cut from the local recording
- [ ] 4K recording where hardware allows

**Done when:** in pilot churches, volunteers using lyric follow miss fewer slide changes, and features run without dropping stream frames on mid-range laptops. Turn off any feature automatically on hardware that can't keep up.

## Stage 5: Multi-campus and large churches (later)

Only after small churches are happy. Some groundwork goes in from day one so this stage isn't a rewrite.

**Build into earlier stages (cheap now, painful later)**

- Data model: Church → Campus → Service from the first migration
- Central plan with campus overrides; locked brand templates
- Output engine behind a clean interface so a native GPU renderer can replace it
- Central health dashboard for all campuses (you need it for support anyway)

**Wait until there's demand**

- [ ] Native GPU renderer for large LED walls (4K+, video backgrounds)
- [ ] NDI and SDI output (check NDI SDK licensing first)
- [ ] Backup computer mirroring with instant failover
- [ ] Campus-to-campus live sermon feed over SRT, direct between campuses
- [ ] Single sign-on, audit exports, annual per-campus contracts, migration service

**Done when:** one large church pilots the coordination features at one campus and agrees to a case study.

## Testing and release process

Reliability comes from testing like it's Sunday every day, and from never shipping risk into a service window.

**Testing**

- **Hardware lab:** at least 3 machines that match real booths — a cheap Intel laptop, a cheap AMD laptop, and a Mac — plus a USB capture card and a USB audio interface.
- **Soak test:** every release candidate runs a 4-hour simulated service (camera, lyrics, stream to a private YouTube event, recording) on all lab machines.
- **Chaos tests:** each item in the failure-mode table above is scripted and run before every release.
- **Automated tests:** unit tests for the command protocol, song parsing and importers; end-to-end tests for the web app.

**Release**

1. Internal build on the lab machines
2. Beta ring: pilot churches opt in and get it midweek
3. Staged rollout: 10% of churches, then everyone a week later if crash reports stay clean
4. Installs only happen outside each church's service windows
5. One-tap rollback to the previous version, kept on disk

**Support on Sundays**

- Crash reports and church heartbeats flow into one dashboard
- Someone is reachable by text during US Sunday morning service hours, even if that's just you at first
- Every Sunday incident gets a short write-up and a test that reproduces it

## Build workflow with Claude Code

This plan is the source of truth for whoever (or whichever Claude session) is building: work one checklist item at a time, in stage order, and check it off here when it's merged.

**Repo layout (one GitHub monorepo)**

- `apps/daemon` — the church-computer app (shell, watchdog, video engine)
- `apps/web` — the web app at app.jivvy.org
- `apps/site` — the website at live.jivvy.org, including the demo
- `packages/protocol` — the versioned command protocol, shared by daemon, web app and the simulated demo daemon
- `packages/songs` — song format, parsers and importers
- `packages/render` — the slide renderer used by screens, the stream layer and the demo

**Rules for every session (put these in the repo's CLAUDE.md)**

1. Read this plan and pick the next unchecked item in the current stage; don't start later stages early.
2. One item per branch and pull request, small enough to review in one sitting.
3. Every change ships with tests; daemon changes also pass the chaos tests in the reliability table.
4. Never break the command protocol: add fields, don't rename or remove them, and keep the previous version working.
5. Nothing during a service may depend on the cloud. If a change would add that dependency, stop and flag it.
6. Keep secrets (stream keys, API keys) out of the repo and out of logs.

**Order to start in**

1. Stage 0 tech spike (daemon prototype: lyrics over camera, hardware encode, YouTube upload)
2. `packages/protocol` and a simulated daemon
3. Website and demo on Cloudflare Pages
4. Stage 1 items

## Costs, pricing and business

Running costs stay around $50–100 a month until you have hundreds of churches, because video never touches your servers.

**Running costs (approximate)**

| Item | Cost |
| --- | --- |
| Supabase Pro ([pricing guide](https://focusreactive.com/blog/supabase-price/)) | $25/month, includes 500 realtime connections and 5M messages |
| Cloudflare R2 media storage ([pricing](https://filebase.com/blog/cloudflare-r2-pricing-costs-savings-and-alternatives-in-2026/)) | $0.015/GB-month, free downloads |
| Web hosting (Cloudflare Pages) | $0–20/month |
| Apple Developer account (Mac app signing) | $99/year |
| Windows code-signing certificate | roughly $200–400/year |
| Email, crash reporting, domain | $0–30/month on starter tiers |
| Stripe fees | about 2.9% + 30¢ per payment |
| Cloudflare Stream (Pro relay only) | $1 per 1,000 minutes sent to platforms, about $1–4 per Pro church per month, plus the account's base subscription |

**Pricing**

- $200 one time per church, unlimited users and devices, includes the first year of Plus and 3 years of updates
- Plus after year one: $60/year, or $80/year billed monthly (about $6.67/month; Stripe's flat 30¢ takes roughly 7% of those small charges)
- Pro (when the relay ships): about $15–20/month; everything in Plus, the cloud relay, and a direct Sunday support line. Kept off the public site until it can be bought
- 30-day free trial

**Licensing and updates**

Goal: be fair to churches. Nothing a church has used in a service ever stops working, and we never charge to keep Sunday running.

| | Included with the $200 | Forever, without Plus | Plus | Pro |
| --- | --- | --- | --- | --- |
| Critical fixes (security, platform changes, anything that breaks a service) | ✅ | ✅ | ✅ | ✅ |
| Certificate renewal for local phone control | ✅ | ✅ | ✅ | ✅ |
| New local features and updates | ✅ first 3 years | Keeps everything released in those 3 years | ✅ ongoing | ✅ ongoing |
| Cloud features (sync, share links, scheduling, backups, alerts, sermon podcast, integrations) | ✅ first year | ❌ | ✅ | ✅ |
| Cloud relay and Sunday support line | ❌ | ❌ | ❌ | ✅ |

- **Everyone runs the same build.** Critical fixes reach every church because nobody is stuck on an old version; the license only decides which features are switched on, by each feature's release date. No backporting.
- **Updates are counted in years, not "major versions,"** so there's never a reason to hold features back.
- **Always free on every license:** everything that runs a service, the local web UI, live captions, the public-domain hymn library and Bible translations, and export of everything.
- **When Plus ends, nothing is lost.** The booth computer keeps a full copy of the library; editing moves to the local UI; share links show "this church's links are paused" instead of breaking.
- **Rejoining Plus** costs $60 with no back-payment, and unlocks everything released in the gap.
- **Reminders** go to the account owner by email and the admin screen, never to volunteers or the screen, and never during a service window.
- **Shutdown promise:** if Jivvy Live ever shuts down, we ship a final update that removes license checks and cloud dependencies so every church keeps working software.
- **Hardship licenses:** free or pay-what-you-can for church plants, very small congregations and overseas missions; larger churches can sponsor one at checkout.

**Cost per Plus church (estimates)**

| Item | Per year |
| --- | --- |
| Recording backup (90 days, 720p, about 20 GB on R2) | ~$4 |
| Sermon podcast audio (about 40 MB per sermon, kept forever) | ~$0.50, growing slowly |
| Database, sync and realtime (Supabase, spread across ~1,000 churches) | ~$1–3 |
| Email, push and alerts | ~$1 |
| Stripe fees | ~$2 (about $6 billed monthly) |
| **Total** | **~$8–10 of $60** |

Servers are cheap; people's time is the real cost. Target margin is 40–50% after support time. That's a deliberate choice to give more back to churches, not a ceiling to push past.

**Support plan**

- The founder handles support and calls at first; hire help (paid) once it outgrows that and revenue supports it.
- Plus support is self-serve first: help center, the support chatbot, email, and booked calls. One-on-one time is saved for Sundays when something is actually wrong.
- Library migration from ProPresenter or EasyWorship is a perk for founding churches; after that the Stage 2 importers do it.
- Sales calls can be handed to someone at about $18/hour; track calls per sale from day one (break-even is about one $200 sale per 10 hours).

**What the money looks like**

| Churches | Upfront (one time) | Plus renewals per year at 60% renewal |
| --- | --- | --- |
| 100 | $20,000 | $3,600 |
| 500 | $100,000 | $18,000 |
| 1,000 | $200,000 | $36,000 |

Growth depends mostly on new sales, since renewals are small. That's the tradeoff for a price churches love.

**Decisions to make**

- [x] Critical streaming and security fixes for churches without Plus: free forever, delivered to everyone on the same build (see Licensing and updates)
- [x] What a license includes without Plus: every local feature released in its first 3 years, kept forever; no cloud features
- [x] Cloud relay: Cloudflare Stream, Pro tier, Stage 3, with automatic fallback to direct streaming
- [ ] Get a publisher quote for licensed Bible translations before promising them
- [ ] Whether to offer a founding-church discount during Stage 0
- [ ] Revenue split, if any, with MinistryBase for customers who come through their integration

## Risks and stop rules

The biggest risk is a bad Sunday; the second is building for months before anyone uses it.

| Risk | What to do about it |
| --- | --- |
| A service fails because of the software | Reliability work comes first in every stage; pilots before public launch; fast incident write-ups |
| Lyrics-over-video compositing is too slow on cheap laptops | Prove it in the Stage 0 spike before anything else; lower default resolution if needed |
| Song licensing | Never scrape or host other people's copyrighted lyrics; churches import what they're licensed for |
| Free tools are "good enough" | Lead with what free tools lack: phone control, volunteer mode, streaming with alerts, reliability |
| Switching is a hassle | Importers for ProPresenter, EasyWorship and OpenLP; offer to migrate a library for early churches |
| Solo Sunday support | Clear status page, in-app troubleshooting, and a limited number of pilot churches until support scales |
| Platform changes (YouTube, Facebook, Windows, macOS) | Watch their developer announcements; keep the Plus renewal funding maintenance |
| Scope creep | Ship Stage 1 before starting anything in Stages 3–5 |
| Pro relay goes down on a Sunday | Automatic fallback to direct streaming, tested with the relay blocked; Pro only launches once Sunday mornings are covered |
| Free-forever fixes outgrow the revenue that pays for them | New sales fund maintenance, with Plus renewals on top; the shutdown promise protects churches if that ever fails |

**Stop rules** — decide these now, before sunk cost decides for you:

- If fewer than 10 churches say they'd switch in Stage 0, change the pitch or stop.
- If the Stage 0 spike can't hold 1080p30 on a $300 laptop, rethink the architecture before building more.
- If fewer than 10 churches run Jivvy Live every Sunday 3 months after Stage 1 ships, pivot or stop.
