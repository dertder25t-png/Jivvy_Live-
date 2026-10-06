# Stage 0 spike: lyrics over video, hardware encode, stream

Throwaway prototype for the Stage 0 tech spike in `docs/BUILD_PLAN.md`. Shell decision: **Tauri** (Oct 6, 2026). Tauri has no offscreen HTML renderer, so the stream's lyric layer is drawn in Rust from the same theme data the web screens use. The Tauri shell itself is not part of this spike yet; this proves the video path it will drive.

## What it does

`source → lyric overlay → Quick Sync H.264 (encode once) → tee → crash-safe MKV recording` and, when a stream key is set, `→ FLV → RTMPS to YouTube`. Silent AAC audio is muxed in so platforms accept the stream. Sample slides are a public-domain hymn. The lyric layer is rendered only when the slide changes (about 1 ms per slide), never per frame.

## Run it (Windows)

Needs Rust (MSVC) and GStreamer 1.28 (`winget install gstreamerproject.gstreamer`; it installs per-user with development files). `run.ps1` sets the GStreamer paths for its own process only.

```powershell
.\run.ps1 test                                   # unit tests
.\run.ps1 run -- --seconds 60                    # test pattern, record to recording.mkv
.\run.ps1 run -- --source webcam --seconds 30    # laptop camera (see known issues)
.\run.ps1 run -Cores 2 -- --seconds 60           # pin to 2 cores to simulate a cheap laptop
```

To stream to YouTube, put the stream key in `spikes/lyrics-stream/.stream-key` (git-ignored) or set `JIVVY_YT_STREAM_KEY`. The key is never printed; errors are scrubbed of it.

## Results so far (Framework 13, Core Ultra X7 358, Arc B390, GStreamer 1.28.6)

| Run | CPU (whole machine) | Frames | Notes |
| --- | --- | --- | --- |
| Test pattern, 1080p30, lyrics, QSV 6 Mbps, MKV + AAC, 60 s | **avg 2.5%, max 7.2%** | 1,802 / 1,800, worst second 28.2 fps | Lyrics verified in the recording |
| Laptop webcam, same pipeline, 30 s | ~0.7% | **~10 fps** | Throttled; see below |

Budget we set for this machine: under ~10% CPU at 1080p30 (the plan's 40% target is for a $300 laptop).

## Known issues (webcam path)

1. **Intermittent crash (heap corruption, `0xC0000374`) when camera capture feeds the Intel encoder.** Camera alone: 6/6 clean. Encoder alone: 6/6 clean. Camera → encoder: about half of runs crash, at startup or at shutdown; when a run starts, it holds 30 fps. Seen with both `mfvideosrc` and the deprecated `ksvideosrc` + `qsvjpegdec`. Handing frames over via `d3d11upload` reduced it (8/8 clean in one batch) but did not eliminate it. Looks like a GStreamer 1.28.6 Windows bug, not our code: it reproduces with `gst-launch-1.0` alone.
2. **10 fps with lyrics on camera frames.** `gst-launch` with the same pipeline and no drawing records all 300 of 300 frames, so the throttle is CPU blending into camera buffers (likely slow-to-map Media Foundation memory).

## Next steps

- Composite the lyric layer on the GPU (`d3d11compositor`: camera pad + lyric pad that only updates on slide change). That fixes issue 2 and is the right production design anyway.
- Pin down issue 1: try GStreamer 1.26, the D3D12 elements, and a crash dump with symbols; report upstream if it holds. Retest with a USB HDMI capture card, which is what most churches will actually use.
- Stream to a private YouTube event once live streaming is enabled on the channel (needs phone verification, then up to 24 h).
- Repeat the measurements pinned to 2 cores, then on a cheap laptop before Stage 1 ships.
