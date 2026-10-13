# Jivvy Live daemon

The church-computer side of Jivvy Live. See `docs/BUILD_PLAN.md` (Stage 1, Daemon).

## Processes

- **`jivvy-watchdog`** starts `jivvy-engine` and `jivvy-outputs` and restarts each one independently when it exits, crashes, or stops sending its heartbeat for 2 seconds (outputs get 10 seconds for their first one, since WebView2 can be slow to start). It holds no state. If the watchdog dies, its children notice their stdin closing and exit, so nothing stale keeps the command port.
- **`jivvy-engine`** holds the live state (current slide, black screen) and answers protocol commands. It saves a snapshot **before acknowledging every change** (stricter than the plan's "every second"): temp file, flush to disk, rename over `state.json`, keeping the last good file as `state.prev.json`. On start it resumes from the newest readable snapshot. Connections that send `state.subscribe` get a `state` event after every change; one that stops reading is disconnected rather than ever slowing the engine.
- **`jivvy-outputs`** shows the state fullscreen on the projector and TVs. It follows the engine over the command channel like any other device, so **an engine restart never blanks a screen**: windows keep the last slide until the engine is back.
- **`jivvy-video`** (built with `--features video`; needs GStreamer) captures the camera or capture card and the audio input, sends audio levels to the engine for live meters, and builds the **program feed**: lyrics composited over the camera and encoded once, for the recording and the stream. Skipped by the watchdog when not installed.

## Output windows

Borderless, always-on-top fullscreen windows placed in physical pixels on each monitor, so per-monitor DPI scaling and monitors left of or above the primary work (the GameWall DisplayHost pattern). Each window is a WebView showing `src/bin/output.html`, which works offline and hides the cursor.

- **Automatic (no config):** every monitor except the primary one (the operator's screen) gets an output at its native resolution. Plug in a projector and it shows.
- **`outputs.json`** in the data directory, for named outputs and custom resolutions:
  ```json
  { "version": 1, "outputs": [
    { "id": "projector", "label": "Main projector", "monitor": "\\\\.\\DISPLAY2" },
    { "id": "led", "label": "LED wall", "monitor": "\\\\.\\DISPLAY3", "width": 1536, "height": 512 }
  ] }
  ```
  `monitor` is a name from `outputs-status.json`, or `"primary"` to use the operator's screen on purpose. With `width` and `height`, the slide is laid out at that size and scaled to fit (black bars); without them it uses the monitor's native size.
- **Never on the operator's screen by accident:** if an output's monitor is missing, the output waits and reappears when the monitor is plugged back in (monitors and config are re-checked every second).
- **A broken `outputs.json` keeps the screens as they were** and is reported as a problem.
- **`outputs-status.json`** lists monitors, outputs, problems, whether the engine is connected, and the state on screen. The System screen will read it.

Until run sheets reach the engine, an output shows "Slide N" (or nothing when black). Configuring outputs from the web app comes with the System screen.

## Camera and audio input

Camera and microphone run as separate GStreamer pipelines, so one failing never stops the other. Today they only capture (and measure); the program feed, recording and stream are built from them (below).

- **Devices:** listed in `media-status.json` with a stable id. Speaker "loopback" inputs are left out, the Windows default microphone is marked `default`, and a camera reported by two Windows APIs is listed once (Media Foundation preferred). Two identical USB cameras stay two.
- **`media.json`** chooses inputs; without it the first camera and the default microphone are used:
  ```json
  { "version": 1,
    "camera": { "use": "device", "id": "<id from media-status.json>", "name": "Blackmagic ATEM" },
    "microphone": { "use": "auto" } }
  ```
  `use` is `auto`, `none`, `test` (test pattern / tone) or `device`.
- **Never a different camera by surprise.** `auto` picks the default (or first) device once and stays on it until `media.json` changes, even across restarts (`media-memory.json`). A configured device is matched by id; by name only when its id changed (e.g. another USB port) and that name has never belonged to two devices at once, so unplugging one of two identical cameras never switches to the other.
- **Unplugged or failed device:** the input shows `waiting` with the reason and is retried every second; it comes back by itself.
- **Device discovery never blocks the main loop.** Devices are listed on their own thread (listing can take a second or more, longer with a bad USB driver); a blocked main loop would miss heartbeats and get the process killed as hung. At start-up the program comes first, then the device monitor.
- **`connected`** in `media-status.json` means the engine acknowledged the latest level report or check (checked at least once a second, even with no microphone).
- **Live meter:** levels (peak and RMS dBFS per channel) reach every subscribed screen as `levels` events. The meter widget comes with the operator view.

## Program feed (what the stream and recording carry)

`camera ─┐                                   ┌─> recording`
`        ├─> pacer ─> lyric layer ─> encode ─┤`
`mic ────┘                                   └─> stream`

- **Never stops because an input does.** The capture pipelines hand frames and audio to a clock-driven pacer that pushes exactly 30 frames a second (configurable) into the program: the newest camera frame, or a black slate when the camera is missing or has sent nothing for 500 ms, and a steady run of audio (the microphone, padded with silence; never more than 200 ms of backlog). Unplugging the camera mid-service switches to the slate without a dropped frame.
- **Lyric layer** drawn natively in Rust (from the spike) only when the text changes, composited on the GPU (Direct3D 12) where available, on the CPU otherwise. Shows "Slide N" until run sheets reach the engine; nothing while the screen is black.
- **Encoded once:** H.264 + AAC, then a tee the recording and the stream will branch from. Encoder order: the configured one first, then Media Foundation, then x264. Each gets 4 seconds to produce a frame; a failure mid-service restarts from the preferred one. Quick Sync only when asked for by name (it crashed in the spike).
- **Settings** in `media.json` (all optional): `"program": { "width": 1920, "height": 1080, "fps": 30, "bitrateKbps": 6000, "encoder": "auto", "path": "auto" }`; `encoder` is `auto`, `mf`, `qsv` or `x264`, `path` is `auto`, `gpu` or `cpu`. Whatever is left out, or `auto`, comes from the hardware check (below); whatever is set wins: that's the tech lead's override.
- **Status** in `media-status.json` → `program`: encoder, picture path (`gpu`/`cpu`), frames per second out, bitrate, whether the slate is on, and the lyric text on screen; `hardware` holds the hardware check's reason in plain words.
- **Cost on the founder's laptop:** the whole video process (camera, microphone, program on Media Foundation) at **0.21 cores, 1.3% of the machine**, 29.8 fps, 6.4 Mbps.

## Recording (crash-safe, local)

`recording.start` and `recording.stop` (protocol commands, saved like the stream's: after an engine or video crash the recording is still wanted and carries on). The engine's state says `recording` (`off`, `recording`, or `waiting` while nothing is being written yet), `recordingWanted` and `recordingLowDisk`; `jivvy-ctl record start|stop` tries it by hand. Nothing here depends on the cloud, and the recorder is a branch like a stream destination: handed copies of the encoded packets on its own thread, never able to slow the program or a stream (if the disk can't keep up, that file is closed and the next one starts).

- **Matroska, written as a stream.** Cluster by cluster, no index or duration to fix up at the end, and every write goes straight to the file, so a killed process leaves a file that plays up to its last moments (the muxer's open cluster, about a second). A part is only reported as `recording` once bytes are really in it.
- **A recording is a set of files.** `recordings/2026-10-10_15-30-12Z/part-001.mkv`, `part-002.mkv`, ... (UTC time of the start; the web app will label them by service). A new part starts whenever the video process starts again while the recording is still wanted, after a failure of the disk or muxer, or when the program changes size or encoder under it. A crash never touches what was already written, and `recording-current.json` tells the next process which set to carry on in. Stopping the recording closes the set.
- **MP4 after the service.** While neither the recording nor the stream is wanted, each part of a finished set is copied (no re-encoding) into `part-001.mp4`, read back, and only if it holds the same frames is the Matroska original removed. It stops the moment a recording or stream is asked for; a part that can't be converted is left as it is. Parts become separate MP4s (joining them into one is a later nicety).
- **Warns at 10 GB free.** `recording.report` carries `lowDisk` to every screen (`recordingLowDisk`) and `media-status.json` → `recording` shows `freeGb`. The recording carries on until the disk is full; then it waits ("The disk is full...") and resumes by itself, in a new file, once there's room.
- **Settings** in `recording.json` (optional): `{ "version": 1, "folder": "D:/Recordings", "warnFreeGb": 10 }`. A relative folder is inside the data folder (default `recordings`). A broken file keeps the last good settings and says so under `problems`.
- **Status** in `media-status.json` → `recording`: `status`, `detail` (plain words), `set`, `file`, `parts`, `bytes`, `freeGb`, `lowDisk`, `converting`, `converted`.
- `jivvy-video --verify-recording FILE` reads a recording back and decodes all of it (exit 0 if it plays): what the chaos tests use to prove no file is ever corrupted.

Not tested: a power cut (the chaos tests kill processes). Data the operating system hadn't flushed to the disk by then is lost, and how the file looks after that depends on the filesystem.

## Hardware check

`jivvy-video --hardware-check [--data-dir DIR]` measures this machine instead of trusting what is installed (a PC without a working GPU still has the Direct3D 12 and Media Foundation plugins, which then run in software at ~7 fps). For each picture path, the graphics chip (D3D12, when its plugins exist) and the processor, and each encoder (Media Foundation, x264; never Quick Sync), it pushes five seconds of moving test picture, with a two-line lyric layer blended onto every frame as on Sunday, through the program's conversion, lyric-layer and encode chain flat out. It picks the first setting, in order of preference (graphics chip before processor, hardware encoder before x264), that sustains **1.5× the frame rate**: 1080p30 if anything does, else 720p30, else 480p30. It writes `hardware.json` (every trial, the choice, and the reason in plain words), prints it, and exits 0; 1 if nothing keeps up; 2 if the report can't be saved. `JIVVY_TEST_NO_GPU=1` and `JIVVY_TEST_ENCODER` narrow what it tries, so a machine with a flaky GPU can be checked safely.

On the founder's laptop (16 cores), the processor path with x264 sustains 131 fps at 1080p with the lyric layer on (145 without it). The slow tier runs the full check with no hooks; on CI's GPU-less Windows runners it has to find the processor path itself.

**Using it.** Before starting `jivvy-video`, the watchdog runs `jivvy-video --hardware-check --if-changed`: it measures when there's no `hardware.json` yet, or when this machine no longer matches the one the report was made on, and otherwise does nothing (about a second). The engine and outputs are already up, so slides never wait; only video does, and only when something changed (event lines `hardware-check started` / `hardware-check finished secs=N result=...`; stopped after 5 minutes if a driver hangs). "Matches" is the report's `fingerprint`: OS and architecture, the processor model and core count, GStreamer's version, the version of each plugin the check measures with (the D3D12 elements, Media Foundation and x264 encoders, the lyric-layer and conversion elements, or `missing`), and the graphics adapters with their driver versions (virtual and remote display adapters, which come and go, are ignored). So a new graphics card or driver, a different processor, a plugin installed, removed or updated on its own, a GStreamer update or a different computer is noticed at the next start. Reports from before fingerprints are measured once more. `jivvy-watchdog --recheck-hardware` measures again whatever the report says; `--no-hardware-check` skips the check and uses the report as it is. An unchanged machine is never re-measured, and a hardware change is only noticed when the watchdog starts, which is why installs and driver updates never happen during a service. The program then takes its encoder and picture path (when `auto`), and its size, frame rate and bitrate (when `media.json` leaves them out) from the report, and with no report it behaves as before. Tried on the founder's laptop: check, then video on the processor with x264 at 1080p, 29.8 fps. The video tests run the check once per run and give every video daemon that report, so CI no longer needs `JIVVY_TEST_ENCODER`/`JIVVY_TEST_NO_GPU`.

## Streaming (RTMP / RTMPS)

`program tee ─> taps ─┬─> destination A: own pipeline ─> flvmux ─> rtmp2sink`
`                     └─> destination B: ...`

- **Isolated from the program.** Each destination has its own small pipeline on its own thread, fed copies of the already-encoded packets. A network failure tears down and reconnects only that destination; the program (and the recording) never notice. A destination that can't keep up is disconnected and reconnects, never slowing the program.
- **Comes back by itself.** Reconnects after 0.5 s, 1 s, 2 s, 3 s, then every 5 s, starting on the next keyframe with timestamps from zero. On the founder's laptop the stream was live again **~2.7 s** after a killed server came back, and **~5–6 s** after the video process itself was killed.
- **"Live" means the server is really getting it,** judged from the RTMP sink's own connection counters, never from buffers reaching the sink (those are accepted before a connection exists). Live needs the server to have answered (handshake and publish), the socket to have taken our bytes for a second, and the server's acknowledgements to keep up (no more than 2 windows behind). Dropped, and reconnected: no answer within 10 s, no bytes taken for 15 s (an overloaded link can take nothing for seconds at a time; that is stepped down, not reconnected), or more than 3 windows unacknowledged. Screens stop showing `live` within 2 s of writes stopping either way.
- **Survives crashes.** `stream.start` / `stream.stop` set "wanted" in the engine, saved like the slide; a restarted engine or video process resumes streaming without anyone touching it.
- **Status everyone can trust.** The video process reports `live` or `reconnecting` once a second (`stream.report`); screens see `live` only while those reports keep coming and every destination is live.
- **Destinations** in `stream.json` (data directory):
  ```json
  { "version": 1, "destinations": [
    { "id": "youtube", "name": "YouTube", "url": "rtmps://a.rtmps.youtube.com:443/live2", "key": "<stream key>" },
    { "id": "facebook", "name": "Facebook", "url": "rtmps://live-api-s.facebook.com:443/rtmp", "key": "<stream key>" }
  ] }
  ```
- **Keys stay secret:** they go only to the RTMP sink, never into a pipeline description, log line, status file or error message (errors are scrubbed, and a broken `stream.json` is reported by line number, never by content). The screen shows the reason in plain words ("Can't reach the streaming server…"); the log keeps the technical one.
- **Video and audio line up by running time,** not raw timestamps: the encoders shift video timestamps (Media Foundation by 1000 hours), and comparing raw ones dropped every audio packet. The tests check that the server gets both tracks.
- Still to come in the streaming item: Advanced mode for the bandwidth manager (below) and two-connection support.

## Streaming to YouTube (HLS segment upload)

`program tee ─> taps ─> segmenter (own pipeline) ─> 2 s .ts files on disk ─> uploader (HTTPS, retries) ─> YouTube`

A destination whose address is `https://` uses YouTube's HLS ingestion instead of RTMP. Paste the address from YouTube Studio (HLS ingestion) and the key:

```json
{ "id": "youtube", "name": "YouTube", "url": "https://a.upload.youtube.com/http_upload_hls?cid=$STREAM_KEY&copy=0&file=", "key": "<stream key>" }
```

- **A drop never loses video.** The segmenter cuts the program into self-contained MPEG-TS segments (one per 2 s keyframe interval: PAT/PMT first, SPS/PPS on the keyframe) on its own thread, whatever the network does. The uploader sends them in order, each after a playlist that lists it, and retries until YouTube confirms (200, or 202 for a segment ahead of its playlist). After a drop the backlog goes out as fast as the connection allows. On the founder's laptop, after a 20 s outage, every segment arrived in order and the stream was live again **~4.7 s** after the connection returned (most of that is the 5 s retry interval).
- **YouTube's ingestion rules** are followed and checked by the tests: the first playlist starts at sequence 0 and sequence numbers only grow; no more than 5 unconfirmed segments listed, plus the last 2 confirmed; segment names unique across restarts (`s<start time>-<n>.ts`); 400 means a file it can't use (skipped, logged), 401 the key wasn't accepted (shown in plain words, retried in case the key is fixed).
- **Survives crashes.** The queue (`stream-spool/<id>/queue.json`, with the waiting segments beside it) is saved after every change. A restarted video process carries on the same playlist with its backlog, timestamps continuing where they stopped. Live again **~10.7 s** after the video process was killed (start-up, then one whole segment has to be cut before it can go). `stream.stop` clears the queue, so the next start is a new broadcast from sequence 0; so does a new key.
- **Never fills the disk.** At most 60 s of video waits (about 45 MB at 6 Mbps); after a longer outage the oldest waiting video is skipped so viewers aren't left minutes behind (the local recording still has it). Confirmed segments are deleted at once.
- **Live** means YouTube has confirmed segments and no more than 6 s of video is waiting; otherwise `reconnecting`, with the reason or how far behind it is.
- **Keys stay secret:** the key goes only into the upload address handed to the HTTP client; status shows `https://a.upload.youtube.com/http_upload_hls`, errors are scrubbed, and the queue file stores a hash to tell keys apart. Plain `http://` is accepted only for this computer (test servers).
- Still to check: a private YouTube event, once the channel can go live.

## Bandwidth manager (Auto and Advanced modes)

`program ─> raw tap (closed unless needed) ─> 720p / 480p / 360p encodes (each its own pipeline, only while used) ─> taps`

- **The recording never gets worse.** The program is always encoded at full quality for the recording (and for destinations the upload can carry). A destination that has to step down gets a lower quality from its own extra encode: 720p (3 Mbps), 480p (1.5 Mbps) or 360p (0.5 Mbps), plus the program's audio (128 kbps). An extra encode starts only while a destination uses it, stops 10 s after the last one leaves, and fails over to the next encoder like the program does; the program's picture reaches them through a valve that is closed (free) otherwise.
- **Measures the upload continuously** from what each connection really carries: for RTMP, bytes handed to the sink minus bytes the socket took (the RTMP sink keeps whatever it can't send, so a slow upload shows there, never as a failed connection); for YouTube HLS, how fast each segment goes up and whether video piles up on disk. A dead connection isn't congestion: it is retried, never answered with a lower quality.
- **A YouTube segment that can't go up in time is congestion, not a failure.** Each segment gets three times its own length (6 s for a 2 s segment, at least 5 s) to go up; one that doesn't, while connected, counts as congestion with an upper bound on the speed, so the manager steps down even before anything has been confirmed (a full-quality segment may never fit). It is retried once, or skipped at once if the destination has stepped down since it was cut. A paused YouTube destination stops uploading straight away and skips what was waiting.
- **Spends about 70%** of the measured upload, keeping the rest to catch up after drops.
- **Auto mode** (the default): destinations are in priority order, the first in `stream.json` being the main platform. The main platform gets the best quality that fits while keeping the lowest for everyone else; when even that doesn't fit, the lowest-priority platform is **paused** first. The main platform is never paused.
- **Down quickly, up carefully.** 3 s of congestion steps down (to what fits the measured speed, possibly several steps at once); 20 s without congestion tries one step up; a step up that congests again doubles the wait before the next try (up to 160 s). Every change costs that platform a reconnect, so changes are at least 8 s apart. A YouTube destination that steps down skips its backlog at the old bitrate (it would take minutes through the slow connection), keeping its sequence numbers correct.
- **Status** (`media-status.json` → `stream`): each destination's `quality` and a plain-words `detail` ("Lowered to 480p: the internet upload is slow. It goes back up by itself." / "Paused: …"), the `uploadKbps` estimate, and the extra `encodes` running. Screens see `live` while every platform that isn't paused is live.
- **Every screen sees each platform, and hears when one is lowered or paused.** The video process's `stream.report` lists each destination (id, name, state, quality); the engine shows them as `streamDestinations` in the state while streaming, and sends subscribers an `alert` event, in plain words, when a platform is lowered, paused, or resumed after a pause ("Facebook is paused: the internet upload can't carry it right now. It comes back by itself when the upload recovers."). Going back up isn't an alert, and nothing is compared across a gap in reports (the video process restarting), so starting or recovering is never mistaken for news. The wording lives in `packages/protocol` (`streamAlerts`) and the Rust port matches it word for word (`fixtures/stream-alerts.json`). Ids in `stream.json` that the protocol wouldn't accept are made safe for the report, so a report is never refused. Push and email to the tech lead come with the alerts item.
- **Advanced mode** (`"bandwidth": {"mode": "advanced"}` in `stream.json`): the church ranks platforms by their order (put Facebook first if it's the main one), and each destination can set `"maxQuality"` (`1080p`, `720p`, `480p`, `360p`) and `"whenShort"`: `"lower"` (the default, as in Auto), `"audioOnly"` (keep its quality while it fits, then the program's sound with a slow picture, 2 frames a second at 100 kbps, since platforms need video to stay live), or `"pause"`. With neither set it behaves exactly like Auto (tested at every speed). Pausing goes from the bottom; the top-ranked platform is paused only if it asked for `"pause"`, and a platform paused on the way comes back if it fits once one above it is paused. A platform at its own top quality isn't shown as lowered. Changing these settings never reconnects a platform. Mistakes (an unknown quality, mode or choice) are listed under `problems` in words and fall back to Auto's behavior; setting them in Auto mode says they only apply in Advanced mode.
- **Pre-flight summary** (`stream.preflight`, or `jivvy-ctl preflight`): the engine tests the upload for about 5 s by sending to our endpoint (`live.jivvy.org/api/upload-test`, which only counts what arrives; `--upload-test-url` points tests elsewhere), then says what each platform will get at that speed with the church's settings, e.g. "YouTube 1080p + Facebook 720p" or "YouTube 360p. Facebook won't fit and would be paused." It comes from the bandwidth manager's own allocation, so it promises what streaming will do. The answer is `measuring` at once; the result follows as a `preflight` event to every subscriber and to whoever asked. The speed is saved (`upload-speed.json`): if the endpoint can't be reached, the last speed measured is used and the message says so. It never runs while streaming, and starting the stream stops a test that's running. Nothing during a service depends on it.

## Command channel

Newline-delimited JSON on `127.0.0.1:47800`, one protocol envelope per line and one ack per line. Every ok ack carries `state`. After `state.subscribe`, the connection also receives `state` events on every change, `levels` events for meters, and `alert` events (a platform lowered, paused or resumed).

**WebSocket** (first step of the secure channel): `jivvy-engine --listen-ws ADDR` (or the same flag on `jivvy-watchdog`, which passes it on) also serves the protocol over WebSocket, off by default. One envelope per text message in; the same acks and events out, one per text message (the TCP lines without the newline); anything else gets the same nacks. The shared scenarios pass over both channels. Browsers send their page's origin, and only origins given with `--ws-origin ORIGIN` (repeatable) may connect, so a web page open on a booth computer can't drive the service; programs (no `Origin` header) always may. A busy WebSocket port is retried in the background and never stops the engine or its TCP channel. Without a certificate, `--listen-ws` only works on a loopback address (development on this computer); any other address is refused with a log line and the WebSocket channel stays off, so pairing codes and proofs never cross the network in plain text. With `--ws-cert PEM --ws-key PEM` the channel is `wss://` only, with that certificate: there is no plain fallback, and with no usable certificate yet, connections are refused until one arrives. Renewing means replacing the two files; each new connection picks the new pair up, so the engine never restarts and open connections stay. A pair that can't be used (half-written mid-renewal, a key that doesn't match) leaves the last good one in use and is logged once, never with the key. Still to come: issuing the per-church hostname and certificate.

**Pairing and device keys.** The local channel (TCP) is the church computer itself and only takes connections from this computer (loopback). `--listen` must be a loopback address (the watchdog and engine refuse anything else at start), since the engine, outputs and video all connect to it. Every WebSocket connection is a device that must be paired, and must prove it holds its key on every connection, before any other command; until then everything else is `unauthorized`.

- A device's identity is a P-256 key it makes itself (WebCrypto, not extractable); the daemon only ever stores public keys, in `devices.json` (saved durably; an unreadable file trusts nobody and is moved aside). `daemon-id` is made once; devices sign it into every proof, so a proof for one church computer is useless on another.
- Pairing only happens while it's open, and only the church computer can open it: `jivvy-ctl pair open` (2 minutes; `jivvy-ctl watch` shows requests). The device sends `pair.request`; both screens show the same 6-digit code; `jivvy-ctl pair approve ID` (or `deny`). At most 5 requests wait.
- On each connection: `auth.hello` returns a one-time challenge, `auth.prove` sends the signature. One try per challenge.
- A paired device runs the service (slides, black, stream and recording on/off, state). Pairing, the device list and the video process's reports are the church computer's alone.
- `jivvy-ctl devices` lists them; `jivvy-ctl devices remove ID` unpairs one and ends its open connections at once.

`src/protocol.rs` is a port of `packages/protocol`, and both are tested against `packages/protocol/fixtures/parse-cases.json`, so they can't drift apart.

## Run

```powershell
cargo run --release --bin jivvy-watchdog -- --slides 20      # data in %LOCALAPPDATA%\JivvyLive
cargo run --release --bin jivvy-watchdog -- --data-dir .\tmp --listen 127.0.0.1:47801
$env:JIVVY_TEST_SOURCES = '1'; .\dev.ps1 run --release --features video --bin jivvy-watchdog   # no camera or microphone needed
```

To try features by hand against a running daemon, `jivvy-ctl` sends one command and prints the engine's answer (the same newline-delimited JSON as every client, on 127.0.0.1:47800 or `--connect ADDR`):

```powershell
.\dev.ps1 ctl next          # also: back, goto 3, black, black off, state, watch, stream start, stream stop, record start, record stop
.\dev.ps1 ctl state
```

`--test-sources` on `jivvy-watchdog` (or `JIVVY_TEST_SOURCES=1`) runs the whole daemon on a GStreamer test pattern and tone for every camera and microphone, whatever `media.json` names, so any machine can run it without a camera, capture card or microphone. An input set to `none` stays off.

## Test

```powershell
.\dev.ps1 check          # before pushing (~30 s): cargo fmt, clippy, FAST tier, npm typecheck and tests; no GStreamer needed
.\dev.ps1 test-full      # once per PR: fmt, clippy and every test with video, engine killed 100 times, npm too (what CI runs)
.\dev.ps1 fast           # FAST tier: unit tests + chaos tests without camera/audio, debug, engine killed 10 times (no GStreamer needed)
.\dev.ps1 slow           # SLOW tier: jivvy-video unit tests + the video and streaming chaos tests (GStreamer + MediaMTX)
.\dev.ps1 test           # every Rust test with video, release, engine killed 100 times (no lint, no npm)
.\dev.ps1 build          # builds jivvy-video too
```

`check` and `test-full` stop at the first failing step and end with a table of each step's time and result, and a line saying what they did not cover. The kill test kills the engine `JIVVY_CHAOS_ITERATIONS` times: 10 by default locally, 100 in CI (GitHub sets `CI`), in `.\dev.ps1 test` / `test-full` and before releases. Use the fast tier while iterating. A fast pass never covers video: run `.\dev.ps1 slow` before any PR that touches video, stream, bandwidth, HLS or tiers code (CI runs it on every PR and nightly either way).

Each chaos test works in `%TEMP%\jivvy-chaos-<test>-<pid>`: deleted when the test passes, kept (path printed) when it fails, with the daemon's log in `daemon.log`. Test ports come from 20000-32767, below the ephemeral range, and a daemon or MediaMTX whose port is taken is started again on another. On Windows the test process puts itself in a job object, so killing it outright (an IDE's stop button, Task Manager) also ends every daemon and MediaMTX it started.

CI runs everything, video and streaming included, on both Windows and Linux. On Windows it installs GStreamer 1.28.6 (the founder's laptop's version, checksum-verified, cached by version) and MediaMTX. Its runners have no GPU, where Media Foundation and Direct3D 12 run in software at ~7 fps at 1080p, so CI sets the test hooks `JIVVY_TEST_ENCODER=x264` and `JIVVY_TEST_NO_GPU=1` (the CPU path and encoder Linux uses anyway). Choosing automatically by measuring whether the GPU path and encoder keep up belongs to the plan's "Encoder self-test and fallback" item.

The streaming tests need [MediaMTX](https://github.com/bluenviron/mediamtx) as a local RTMP server, in `apps/daemon/.tools/mediamtx/` (git-ignored) or at `$JIVVY_MEDIAMTX`. To get the same release CI uses (checksum checked):

```bash
mkdir -p .tools/mediamtx && cd .tools/mediamtx && gh release download v1.21.1 --repo bluenviron/mediamtx --pattern "mediamtx_v1.21.1_windows_amd64.zip" --pattern checksums.sha256 && grep windows_amd64.zip checksums.sha256 | sha256sum -c - && unzip -o mediamtx_v1.21.1_windows_amd64.zip
```

Chaos tests (`tests/chaos.rs`), covering the reliability table's "Video engine crashes" row, part of "Corrupted settings", and the output windows (run with `--outputs-headless`, which reads monitors from `test-displays.json`):

| Test | Result on the founder's laptop (Windows) |
| --- | --- |
| Kill the engine 100 times, changing slide and black state between kills | Back on the same slide every time; slowest 1.68 s (target under 3 s). A single crash after a long run restarts in about 0.2–0.3 s; repeated crashes back off up to 1 s. |
| Hang the engine (deadlock while holding its state lock) | Killed after 2 s without a heartbeat and resumed on the same slide in under 3.5 s |
| Kill the watchdog | Engine exits within 3 s, freeing the port |
| Corrupt `state.json`, then kill the engine | Resumes from `state.prev.json` |
| Kill the engine while outputs show slide 6 | Outputs keep showing it (never blank), reconnect, and follow the restarted engine; the outputs process itself is not restarted |
| Kill the outputs process | Back on the current slide in under 3 s |
| Unplug the projector, plug it back, then save a broken `outputs.json` | Output closes (never moves to the operator's screen), reopens, and keeps running on the last good config |
| Kill the video process (test pattern + tone) | Restarted; the program is fed again first, then the inputs; audio levels reach screens again about 4–6 s after the kill (most of it is GStreamer loading its GPU and encoder plugins) |
| Configured camera missing | Camera `waiting`, audio keeps running; the camera starts within a second of becoming available |
| Program on test sources | Runs at > 25 fps, lyric layer follows slide changes and black |
| Camera unplugged and microphone removed mid-program | Slate on, still ~30 fps (≥ 50 frames in 2 s), same encoder, never restarted; off the slate when the camera returns |
| Change the program size mid-run (1080p → 720p) | Back running at the new size on the camera at full rate, no encoder failure, and the video process is not restarted |
| Stream to MediaMTX, kill the server, bring it back | Screens see `reconnecting` (plain-words reason), the program stays at ~30 fps, and the stream is live again by itself (~2.7 s after the server returns); the video process never restarts |
| Kill the video process while streaming, then stop | Stream resumes by itself (~6 s); after `stream.stop` the server stops receiving |
| Stream to a port nothing listens on | `reconnecting` with "Can't reach the streaming server", retries counted, the key in no status or state file |
| Stream to a server that accepts the connection but never answers | Never shown live; gives up after 10 s with "Can't reach the streaming server" and keeps retrying |
| YouTube HLS (fake ingest that enforces YouTube's rules): connection dead for 20 s | Segments keep being cut to disk; `reconnecting` in plain words; program at ~30 fps; every segment arrives in order once it's back, caught up and live ~4.7 s later; each segment decodes on its own with picture (2 s at 30 fps) and sound |
| YouTube HLS: kill the video process, then stop and start again | Same playlist carried on (sequence numbers only grow, no name reused), live ~10.7 s after the kill; nothing sent after stop; the next start begins at sequence 0 |
| YouTube HLS: key not accepted (401) | `reconnecting`, "The platform rejected the stream…", retries counted, the key in no status, state or queue file |
| Slow upload: RTMP through a proxy throttled to 1 Mbps, then unthrottled | Live at 360p ~10 s after the drop ("Lowered to 360p…", upload estimate ~1 Mbps); the program stays at full size, rate and bitrate; back up a step ~20 s after the upload returns; the video process never restarts |
| Two platforms on one 1.2 Mbps upload (YouTube HLS first, Facebook RTMP second) | Facebook paused ("Paused: …"), YouTube live at 360p within YouTube's ingestion rules; screens show `live` |
| YouTube only, on a 900 kbps link: a full-quality segment can't go up in the 6 s it gets | Steps down before anything is confirmed and is live at 360p ~27 s after starting; within YouTube's ingestion rules |
| Facebook first, YouTube (HLS) second on a shared 1.2 Mbps upload | YouTube paused; nothing more is uploaded and nothing waits on disk |

The bandwidth tests stream a 720p, 3 Mbps program of a noisy test picture (`JIVVY_TEST_PATTERN=snow`), so every encoder really spends its bitrate as on a camera; x264 squeezes the usual test pattern to almost nothing.

The video tests run one daemon at a time: a church computer runs one program, and several 1080p30 encodes sharing a laptop's hardware encoder held it to ~20 fps.

On the founder's laptop with real devices: Laptop Camera at 30 fps, the microphone array reporting levels ten times a second, the speaker loopback and duplicate camera entries filtered out.
