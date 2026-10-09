# Daemon dev commands. GStreamer's paths are set for this process only, and only for the
# commands that need the `video` feature (jivvy-video). Usage: .\dev.ps1 <command>
#   .\dev.ps1 fast      -> FAST tier: unit tests (lib + bins, not jivvy-video) and the chaos tests without video,
#                          debug build, kill test at JIVVY_CHAOS_ITERATIONS (default 10). No GStreamer.
#   .\dev.ps1 slow      -> SLOW tier: jivvy-video unit tests and the video and streaming chaos tests
#                          (release, --features video)
#   .\dev.ps1 build     -> cargo build --release --features video
#   .\dev.ps1 test      -> FULL: cargo test --release --features video, engine killed 100 times
#   .\dev.ps1 <args>    -> cargo <args> with GStreamer's paths set
$Rest = @($args)

function Use-GStreamer {
  $gst = $env:GSTREAMER_1_0_ROOT_MSVC_X86_64
  if (-not $gst) { $gst = Join-Path $env:LOCALAPPDATA 'Programs\gstreamer\1.0\msvc_x86_64' }
  if (-not (Test-Path "$gst\bin\gst-launch-1.0.exe")) { throw "GStreamer not found at $gst. Install it (winget install gstreamerproject.gstreamer) or set GSTREAMER_1_0_ROOT_MSVC_X86_64." }
  $env:PATH = "$gst\bin;$env:PATH"
  $env:PKG_CONFIG = "$gst\bin\pkg-config.exe"
  $env:PKG_CONFIG_PATH = "$gst\lib\pkgconfig"
}

function Show-Tier($name, $covers, $skips) {
  Write-Host ""
  Write-Host "== $name tier: $covers" -ForegroundColor Cyan
  if ($skips) { Write-Host "   NOT covered: $skips" -ForegroundColor Yellow }
}

Push-Location $PSScriptRoot
try {
  switch ($Rest[0]) {
    'fast' {
      $kills = if ($env:JIVVY_CHAOS_ITERATIONS) { $env:JIVVY_CHAOS_ITERATIONS } else { '10' }
      Show-Tier 'FAST' "unit tests (lib + bins, not jivvy-video) and chaos tests without video, engine killed $kills times" 'video and streaming tests, jivvy-video unit tests (run .\dev.ps1 slow)'
      cargo test @($Rest | Select-Object -Skip 1)
      $code = $LASTEXITCODE
      Show-Tier 'FAST' "finished, exit $code" 'video and streaming tests, jivvy-video unit tests (run .\dev.ps1 slow)'
      exit $code
    }
    'slow' {
      Use-GStreamer
      Show-Tier 'SLOW' 'jivvy-video unit tests, then the video and streaming chaos tests (GStreamer test sources, MediaMTX, fake YouTube)' $null
      # Extra args go to the test binaries, e.g. .\dev.ps1 slow --nocapture
      cargo test --release --features video --bin jivvy-video -- @($Rest | Select-Object -Skip 1)
      $code = $LASTEXITCODE
      if ($code -eq 0) {
        cargo test --release --features video --test chaos -- video:: @($Rest | Select-Object -Skip 1)
        $code = $LASTEXITCODE
      }
      Show-Tier 'SLOW' "finished, exit $code" $null
      exit $code
    }
    'build' { Use-GStreamer; cargo build --release --features video }
    'test' {
      Use-GStreamer
      if (-not $env:JIVVY_CHAOS_ITERATIONS) { $env:JIVVY_CHAOS_ITERATIONS = '100' }
      Show-Tier 'FULL' "everything, release build, engine killed $env:JIVVY_CHAOS_ITERATIONS times" $null
      cargo test --release --features video @($Rest | Select-Object -Skip 1)
    }
    default { Use-GStreamer; cargo @Rest }
  }
  exit $LASTEXITCODE
} finally { Pop-Location }
