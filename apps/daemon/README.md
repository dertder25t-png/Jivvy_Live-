# Jivvy Live daemon

The church-computer side of Jivvy Live. See `docs/BUILD_PLAN.md` (Stage 1, Daemon).

## Processes

- **`jivvy-watchdog`** starts `jivvy-engine` and restarts it when it exits, crashes, or stops sending its heartbeat for 2 seconds. It holds no state. If the watchdog dies, the engine notices its stdin closing and exits, so it never keeps the command port from the next engine.
- **`jivvy-engine`** holds the live state (current slide, black screen) and answers protocol commands. It saves a snapshot **before acknowledging every change** (stricter than the plan's "every second"): temp file, flush to disk, rename over `state.json`, keeping the last good file as `state.prev.json`. On start it resumes from the newest readable snapshot.

Video, output windows, recording and streaming plug into the engine in later Stage 1 items. Until streaming lands, `stream.start` and `stream.stop` answer `unavailable`.

## Command channel (for now)

Newline-delimited JSON on `127.0.0.1:47800`, one protocol envelope per line and one ack per line. Every ok ack carries `state`. The secure per-church WebSocket channel is a later Stage 1 item; the envelope and ack formats stay the same.

`src/protocol.rs` is a port of `packages/protocol`, and both are tested against `packages/protocol/fixtures/parse-cases.json`, so they can't drift apart.

## Run

```powershell
cargo run --release --bin jivvy-watchdog -- --slides 20      # data in %LOCALAPPDATA%\JivvyLive
cargo run --release --bin jivvy-watchdog -- --data-dir .\tmp --listen 127.0.0.1:47801
```

## Test

```powershell
cargo test --release      # unit tests + chaos tests (about 3 minutes)
```

Chaos tests (`tests/chaos.rs`), covering the reliability table's "Video engine crashes" row and part of "Corrupted settings":

| Test | Result on the founder's laptop (Windows) |
| --- | --- |
| Kill the engine 100 times, changing slide and black state between kills | Back on the same slide every time; slowest 1.68 s (target under 3 s). A single crash after a long run restarts in about 0.2–0.3 s; repeated crashes back off up to 1 s. |
| Hang the engine (deadlock while holding its state lock) | Killed after 2 s without a heartbeat and resumed on the same slide in under 3.5 s |
| Kill the watchdog | Engine exits within 3 s, freeing the port |
| Corrupt `state.json`, then kill the engine | Resumes from `state.prev.json` |
