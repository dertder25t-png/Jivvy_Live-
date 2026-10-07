# Jivvy Live daemon

The church-computer side of Jivvy Live. See `docs/BUILD_PLAN.md` (Stage 1, Daemon).

## Processes

- **`jivvy-watchdog`** starts `jivvy-engine` and `jivvy-outputs` and restarts each one independently when it exits, crashes, or stops sending its heartbeat for 2 seconds (outputs get 10 seconds for their first one, since WebView2 can be slow to start). It holds no state. If the watchdog dies, its children notice their stdin closing and exit, so nothing stale keeps the command port.
- **`jivvy-engine`** holds the live state (current slide, black screen) and answers protocol commands. It saves a snapshot **before acknowledging every change** (stricter than the plan's "every second"): temp file, flush to disk, rename over `state.json`, keeping the last good file as `state.prev.json`. On start it resumes from the newest readable snapshot. Connections that send `state.subscribe` get a `state` event after every change; one that stops reading is disconnected rather than ever slowing the engine.
- **`jivvy-outputs`** shows the state fullscreen on the projector and TVs. It follows the engine over the command channel like any other device, so **an engine restart never blanks a screen**: windows keep the last slide until the engine is back.
- **`jivvy-video`** (built with `--features video`; needs GStreamer) captures the camera or capture card and the audio input, sends audio levels to the engine for live meters, and builds the **program feed**: lyrics composited over the camera and encoded once, for the recording and the stream. Skipped by the watchdog when not installed.

Video, recording and streaming plug into the engine in later Stage 1 items. Until streaming lands, `stream.start` and `stream.stop` answer `unavailable`.

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

Camera and microphone run as separate GStreamer pipelines, so one failing never stops the other. Today they only capture (and measure); the lyric layer, encoder, recording and stream attach to them in the next Stage 1 items.

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
- **`connected`** in `media-status.json` means the engine acknowledged the latest level report or check (checked at least once a second, even with no microphone).
- **Live meter:** levels (peak and RMS dBFS per channel) reach every subscribed screen as `levels` events. The meter widget comes with the operator view.

## Program feed (what the stream and recording carry)

`camera ─┐                                   ┌─> recording (next item)`
`        ├─> pacer ─> lyric layer ─> encode ─┤`
`mic ────┘                                   └─> stream (item after)`

- **Never stops because an input does.** The capture pipelines hand frames and audio to a clock-driven pacer that pushes exactly 30 frames a second (configurable) into the program: the newest camera frame, or a black slate when the camera is missing or has sent nothing for 500 ms, and a steady run of audio (the microphone, padded with silence; never more than 200 ms of backlog). Unplugging the camera mid-service switches to the slate without a dropped frame.
- **Lyric layer** drawn natively in Rust (from the spike) only when the text changes, composited on the GPU (Direct3D 12) where available, on the CPU otherwise. Shows "Slide N" until run sheets reach the engine; nothing while the screen is black.
- **Encoded once:** H.264 + AAC, then a tee the recording and the stream will branch from. Encoder order: the configured one first, then Media Foundation, then x264. Each gets 4 seconds to produce a frame; a failure mid-service restarts from the preferred one. Quick Sync only when asked for by name (it crashed in the spike).
- **Settings** in `media.json` (all optional): `"program": { "width": 1920, "height": 1080, "fps": 30, "bitrateKbps": 6000, "encoder": "auto" }` (`auto`, `mf`, `qsv`, `x264`).
- **Status** in `media-status.json` → `program`: encoder, frames per second out, bitrate, whether the slate is on, and the lyric text on screen.
- **Cost on the founder's laptop:** the whole video process (camera, microphone, program on Media Foundation, plus a debug file) at **0.21 cores, 1.3% of the machine**, 29.8 fps, 6.4 Mbps.
- Developer aid until the recording item lands: `JIVVY_DEBUG_PROGRAM_FILE=path.mkv` also writes the program to a file.

## Command channel (for now)

Newline-delimited JSON on `127.0.0.1:47800`, one protocol envelope per line and one ack per line. Every ok ack carries `state`. After `state.subscribe`, the connection also receives `state` events on every change and `levels` events for meters. The secure per-church WebSocket channel is a later Stage 1 item; the envelope and ack formats stay the same.

`src/protocol.rs` is a port of `packages/protocol`, and both are tested against `packages/protocol/fixtures/parse-cases.json`, so they can't drift apart.

## Run

```powershell
cargo run --release --bin jivvy-watchdog -- --slides 20      # data in %LOCALAPPDATA%\JivvyLive
cargo run --release --bin jivvy-watchdog -- --data-dir .\tmp --listen 127.0.0.1:47801
```

## Test

```powershell
cargo test --release      # unit tests + chaos tests (about 3 minutes), without camera/audio
.\dev.ps1 test           # the same plus the video tests (sets GStreamer's paths for this process)
.\dev.ps1 build          # builds jivvy-video too
```

CI runs the video tests on Linux; the Windows CI job builds without the `video` feature until GStreamer is installed there.

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
| Kill the video process (test pattern + tone) | Restarted; audio levels reach screens again 2.3 s after the kill |
| Configured camera missing | Camera `waiting`, audio keeps running; the camera starts within a second of becoming available |
| Program on test sources | Runs at > 25 fps, lyric layer follows slide changes and black |
| Camera unplugged and microphone removed mid-program | Slate on, still ~30 fps (≥ 50 frames in 2 s), same encoder, never restarted; off the slate when the camera returns |

On the founder's laptop with real devices: Laptop Camera at 30 fps, the microphone array reporting levels ten times a second, the speaker loopback and duplicate camera entries filtered out.
