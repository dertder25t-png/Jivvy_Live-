# Stage 0 spike: lyrics over video, hardware encode, stream

Throwaway prototype for the Stage 0 tech spike in `docs/BUILD_PLAN.md`. Shell decision: **Tauri** (Oct 6, 2026). Tauri has no offscreen HTML renderer, so the stream's lyric layer is drawn in Rust from the same theme data the web screens use. The Tauri shell itself is not part of this spike yet; this proves the video path it will drive.

## What it does

`source → GPU upload → lyric layer composited on the GPU → H.264 hardware encode (once) → tee → crash-safe MKV recording` and, when a stream key is set, `→ FLV → RTMPS to YouTube`. Silent AAC audio is muxed in so platforms accept the stream. Sample slides are a public-domain hymn.

The lyric layer is drawn on the CPU **only when the slide changes** (about 1 ms per slide). `overlaycomposition` attaches it to each frame as metadata, and `d3d12overlaycompositor` blends it on the GPU, so the CPU never touches a full video frame.

## Run it (Windows)

Needs Rust (MSVC) and GStreamer 1.28 (`winget install gstreamerproject.gstreamer`; it installs per-user with development files). `run.ps1` sets the GStreamer paths for its own process only.

```powershell
.\run.ps1 test                                                # unit tests
.\run.ps1 run -- --seconds 60                                 # test pattern, record to recording.mkv
.\run.ps1 run -- --source webcam --encoder mf --seconds 30    # laptop camera, Media Foundation encoder
.\run.ps1 run -Cores 2 -- --source webcam --encoder mf        # pin to 2 cores to simulate a cheap laptop
```

`--encoder qsv` (default) uses GStreamer's Quick Sync plugin; `--encoder mf` uses Windows Media Foundation's Intel H.264 encoder. See known issues before choosing.

To stream to YouTube, put the stream key in `spikes/lyrics-stream/.stream-key` (git-ignored) or set `JIVVY_YT_STREAM_KEY`. The key is never printed; errors are scrubbed of it.

## Results (Framework 13, Core Ultra X7 358, Arc B390, Intel driver 32.0.101.8860, GStreamer 1.28.6)

All at 1080p30, lyrics on, 6 Mbps H.264, MKV with AAC.

| Run | CPU | Frames | Stability |
| --- | --- | --- | --- |
| Test pattern, QSV, 60 s | avg 2.5%, max 7.2% of machine | 1,802 / 1,800 | |
| Webcam, CPU lyric blending (first version) | ~0.7% | **~10 fps** | Fixed by GPU compositing |
| **Webcam, GPU lyrics, Media Foundation encoder, 30 s** | **avg 0.8%, max 1.7% of machine** | ~878 / 900 (first second is camera warm-up), worst second 28 fps | **13 / 13 runs clean** |
| Webcam, GPU lyrics, MF, pinned to 2 cores | ~6% of those 2 cores | 878 / 900 | clean |
| Webcam, GPU lyrics, QSV | ~0.7% | 880 / 900 when it runs | crashed 2 of 3 |

Recording checked: 21.5 MB for 30 s (matches 6 Mbps), lyrics correctly placed over the camera image.

Budgets: under ~10% CPU on this machine; the plan's target is under 40% on a $300 laptop.

## Known issues

### 1. GStreamer's Quick Sync plugin crashes on this machine (not our code)

`STATUS_HEAP_CORRUPTION` (`0xC0000374`, reported in `ntdll.dll`) in roughly 20–40% of process launches whenever the `qsv` plugin loads. Narrowed down with `gst-launch-1.0` / `gst-inspect-1.0` only:

| Repro (10 runs each) | 1.28.6 | 1.26.11 |
| --- | --- | --- |
| `gst-inspect-1.0 qsvh264enc` (just loads the plugin) | 4 crashes | |
| `videotestsrc ! qsvh264enc ! fakesink` (no camera) | 2 crashes | 2 crashes |
| `mfvideosrc ! qsvh264enc ! fakesink` | 3 crashes | 3 crashes |
| `gst-inspect-1.0` of `d3d12h264enc`, `mfh264enc`, `d3d11upload` | 0 | |
| `mfvideosrc ! d3d12upload ! d3d12h264enc` | 0 | |
| Media Foundation encoder (`mfh264enc`), 25 runs incl. full spike | 0 | |

So it is the Quick Sync plugin's start-up (where it enumerates Intel's oneVPL runtimes), not the camera and not a 1.28 regression. The process loads the current driver's `libmfx64-gen.dll` (26.05) plus Intel's MFX Loader `System32\libmfxhw64.dll` (23.06), which the same driver installs. Next step for an upstream report: a crash dump with symbols (WinDbg or ProcDump) and a page-heap run to catch the code that corrupts the heap. It may belong to GStreamer's `qsv` plugin or to Intel's runtime for this new GPU.

### 2. Direct3D 12 encoder output is wrong on this GPU

`d3d12h264enc` never crashes but encodes only one frame in four (the others are 6-byte empty frames) and ignores the CBR bitrate, even with moving test video. Not usable here; worth retesting on newer drivers.

### Design takeaways for the daemon

- **Composite lyrics on the GPU** and draw the lyric layer only on slide change.
- **Treat the encoder as something that can fail on a church's PC.** Run a quick encoder self-test in pre-flight, keep a fallback order (here: Media Foundation → Quick Sync → x264), and run the encoder where the watchdog can restart it without losing the recording.
- Camera capture already decodes to NV12 through Media Foundation, so a USB camera (or an ATEM Mini, which appears to Windows as a webcam) needs no CPU conversion.

## Next steps

- Stream to a private YouTube event once live streaming is enabled on the channel (needs phone verification, then up to 24 h).
- File the drafted report ([crash-report.md](crash-report.md)) with Intel and GStreamer once approved.
- Retest Quick Sync and D3D12 encode after the next Intel driver update.
- Measure the x264 software fallback's CPU cost.
- Repeat on a cheap laptop before Stage 1 ships.
