# Draft bug report: heap corruption when the GStreamer `qsv` plugin loads on Intel Arc B390

Draft for Intel (VPL GPU runtime / graphics driver) with a copy to GStreamer. **Not filed yet.** Only post it publicly with the founder's OK.

## Summary

On an Intel Core Ultra X7 358H with Arc B390 graphics, loading GStreamer's `qsv` plugin crashes the process with `STATUS_HEAP_CORRUPTION` (`0xC0000374`) in roughly 20–40% of launches. No video needs to be processed: `gst-inspect-1.0 qsvh264enc` alone reproduces it. Under a debugger, the corruption is detected while unwinding a C++ exception that `d3d11.dll` throws when `CreateVideoDecoder` fails during the runtime's capability probe.

## Environment

| | |
| --- | --- |
| CPU / GPU | Intel Core Ultra X7 358H, Intel Arc B390 GPU |
| Graphics driver | 32.0.101.8860 (`igd10umt64xe.dll`, `igd11dxva64.dll` same version) |
| VPL GPU runtime | `libmfx64-gen.dll` 26.05.06.7eb2a756 (from the driver package) |
| OS | Windows 11 Pro build 26200.9457, `d3d11.dll` 10.0.26100.9278 |
| GStreamer | 1.28.6 and 1.26.11 (official MSVC x86_64 builds); same results on both |

## Reproduce

```powershell
1..10 | % { (Start-Process gst-inspect-1.0.exe -ArgumentList qsvh264enc -NoNewWindow -PassThru -Wait).ExitCode }
```

Result: 4 of 10 exit with `-1073740940` (`0xC0000374`). Loading other hardware plugins (`d3d12h264enc`, `mfh264enc`, `d3d11upload`) exits cleanly 10 of 10.

## Stack (cdb, heap validation on free)

```
ntdll!RtlpBreakPointHeap+0x16
ntdll!RtlpCheckBusyBlockTail+0x232        <- block tail overwritten
ntdll!RtlpValidateHeapEntry+0x177
ntdll!RtlDebugFreeHeap+0xb4
ntdll!RtlpFreeHeap+0x9f7
ntdll!RtlFreeHeap+0x620
d3d11!operator delete+0x28
d3d11!std::unique_ptr<void,std::default_delete<void> >::~unique_ptr+0x14
ucrtbase!...FrameUnwindToState / _CxxFrameHandler4 (unwinding)
ntdll!RtlUnwindEx+0x300
...
KERNELBASE!RaiseException+0x8a
ucrtbase!CxxThrowException+0x96
d3d11!ThrowFailure+0x32
d3d11!CUseCountedObject<NOutermost::CDeviceChild>::CUseCountedObject+0xab
d3d11!NOutermost::CDevice::CreateLayeredChild+0x166
d3d11!CDevice::CreateVideoDecoder+0xe2
libmfx64_gen!MFXVideoVPP_RunFrameVPPAsyncEx+0x244978   (no public symbols)
libmfx64_gen!MFXVideoVPP_RunFrameVPPAsyncEx+0x2405d8
libmfx64_gen!MFXVideoVPP_RunFrameVPPAsyncEx+0x1d84ee
libmfx64_gen+0x56332
gstqsv+0x2ca7a
gstqsv+0x2e1b4
gstreamer_1_0_0!gst_plugin_load_file+0x2a7
...
gst_inspect_1_0
```

A full-memory dump is available on request (it contains no personal data beyond the process's own memory, but is ~340 MB).

## What we observed

- `libmfx64_gen` calls `ID3D11VideoDevice::CreateVideoDecoder` repeatedly while the plugin enumerates codecs; many calls fail and `d3d11.dll` throws (visible as repeated first-chance `e06d7363` C++ exceptions). Only some failures end in corruption.
- With full page heap enabled for `gst-inspect-1.0.exe` (`GlobalFlag 0x02000000`, `PageHeapFlags 0x3`), 12 of 12 runs were clean, so the corruption looks timing- or layout-dependent rather than a simple linear overrun.
- Media Foundation's Intel H.264 encoder MFT on the same machine runs cleanly (25/25), as does `d3d12h264enc` (which, separately, produces mostly empty frames and ignores CBR bitrate on this GPU).

## Suspected owner

The failing call and the corrupted allocation are inside the D3D11 video-decoder creation path driven by Intel's VPL GPU runtime. Likely an Intel driver/runtime issue on this new GPU; GStreamer's `qsv` plugin only triggers it by probing all codecs at plugin load.
