# Daemon dev commands. GStreamer's paths are set for this process only, and only for the
# commands that need the `video` feature (jivvy-video). Usage: .\dev.ps1 <command>
#   .\dev.ps1 check     -> before pushing: cargo fmt, clippy, FAST tier, npm run typecheck, npm test.
#                          Says at the end what it did not cover. No GStreamer needed.
#   .\dev.ps1 test-full -> everything, once per PR: fmt, clippy and all tests with video, engine
#                          killed 100 times, plus npm typecheck and tests (GStreamer + MediaMTX)
#   .\dev.ps1 fast      -> FAST tier: unit tests (lib + bins, not jivvy-video) and the chaos tests without video,
#                          debug build, kill test at JIVVY_CHAOS_ITERATIONS (default 10). No GStreamer.
#   .\dev.ps1 slow      -> SLOW tier: jivvy-video unit tests and the video and streaming chaos tests
#                          (release, --features video)
#   .\dev.ps1 ctl next  -> jivvy-ctl: one-line commands to a running daemon (next, back, black, state, ...)
#   .\dev.ps1 build     -> cargo build --release --features video
#   .\dev.ps1 test      -> cargo test --release --features video, engine killed 100 times (Rust only)
#   .\dev.ps1 <args>    -> cargo <args> with GStreamer's paths set
# PowerShell drops a bare -- from a script's arguments: quote it, e.g. .\dev.ps1 fast '--' --nocapture
$Rest = @($args)
$RepoRoot = Resolve-Path (Join-Path $PSScriptRoot '..\..')

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

# Runs named steps in order, stopping at the first failure, then prints each step's time and
# result and what the run did not cover. Returns the exit code.
function Invoke-Steps($title, $steps, $skips) {
  $ran = @()
  $code = 0
  foreach ($s in $steps) {
    Write-Host ""
    Write-Host "-- $($s.Name)" -ForegroundColor Cyan
    $sw = [Diagnostics.Stopwatch]::StartNew()
    & $s.Run | Out-Host
    $code = $LASTEXITCODE
    $result = if ($code -eq 0) { 'ok' } else { "FAILED (exit $code)" }
    $ran += [pscustomobject]@{ Step = $s.Name; Seconds = [math]::Round($sw.Elapsed.TotalSeconds, 1); Result = $result }
    if ($code -ne 0) { break }
  }
  $total = ($ran | Measure-Object Seconds -Sum).Sum
  $color = if ($code -eq 0) { 'Green' } else { 'Red' }
  Write-Host ""
  Write-Host "== $title $(if ($code -eq 0) { 'passed' } else { 'FAILED' }) in $total s" -ForegroundColor $color
  $ran | Format-Table -AutoSize | Out-Host
  if ($skips) { Write-Host "   NOT covered: $skips" -ForegroundColor Yellow }
  return $code
}

function Step($name, [scriptblock]$run) { [pscustomobject]@{ Name = $name; Run = $run } }

function Invoke-Npm($script) {
  Push-Location $RepoRoot
  try { npm run $script } finally { Pop-Location }
}

Push-Location $PSScriptRoot
try {
  switch ($Rest[0]) {
    'check' {
      $kills = if ($env:JIVVY_CHAOS_ITERATIONS) { $env:JIVVY_CHAOS_ITERATIONS } else { '10' }
      $code = Invoke-Steps 'CHECK (fast tier)' @(
        (Step 'cargo fmt --check' { cargo fmt --check }),
        (Step 'cargo clippy (no video)' { cargo clippy --all-targets -q -- -D warnings }),
        (Step "FAST tier: unit + chaos tests without video, engine killed $kills times" { cargo test }),
        (Step 'npm run typecheck' { Invoke-Npm typecheck }),
        (Step 'npm test' { Invoke-Npm test })
      ) 'video and streaming tests and jivvy-video unit tests (.\dev.ps1 slow), clippy with video, the full 100 kills (.\dev.ps1 test-full)'
      exit $code
    }
    'test-full' {
      Use-GStreamer
      if (-not $env:JIVVY_CHAOS_ITERATIONS) { $env:JIVVY_CHAOS_ITERATIONS = '100' }
      $kills = $env:JIVVY_CHAOS_ITERATIONS
      $skips = if ($kills -ne '100') { "the full 100 kills (JIVVY_CHAOS_ITERATIONS is $kills)" } else { $null }
      $code = Invoke-Steps 'TEST-FULL (everything)' @(
        (Step 'cargo fmt --check' { cargo fmt --check }),
        (Step 'cargo clippy (with video)' { cargo clippy --release --all-targets --features video -q -- -D warnings }),
        (Step "all Rust tests with video, release, engine killed $kills times" { cargo test --release --features video }),
        (Step 'npm run typecheck' { Invoke-Npm typecheck }),
        (Step 'npm test' { Invoke-Npm test })
      ) $skips
      exit $code
    }
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
    'ctl' { cargo run -q --bin jivvy-ctl -- @($Rest | Select-Object -Skip 1) }
    'build' { Use-GStreamer; cargo build --release --features video }
    'test' {
      Use-GStreamer
      if (-not $env:JIVVY_CHAOS_ITERATIONS) { $env:JIVVY_CHAOS_ITERATIONS = '100' }
      $extra = @($Rest | Select-Object -Skip 1)
      $scope = if ($extra.Count) { "only: $($extra -join ' ')" } else { 'all Rust tests (npm and lint: .\dev.ps1 test-full)' }
      Show-Tier 'FULL' "release build with video, engine killed $env:JIVVY_CHAOS_ITERATIONS times; $scope" $null
      cargo test --release --features video @extra
    }
    default { Use-GStreamer; cargo @Rest }
  }
  exit $LASTEXITCODE
} finally { Pop-Location }
