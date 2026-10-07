# Runs cargo with GStreamer's paths set for this process only, so the `video` feature
# (jivvy-video) builds and runs. Usage: .\dev.ps1 build | test | <any cargo args>
#   .\dev.ps1 test      -> cargo test --release --features video
$Rest = @($args)
$gst = $env:GSTREAMER_1_0_ROOT_MSVC_X86_64
if (-not $gst) { $gst = Join-Path $env:LOCALAPPDATA 'Programs\gstreamer\1.0\msvc_x86_64' }
if (-not (Test-Path "$gst\bin\gst-launch-1.0.exe")) { throw "GStreamer not found at $gst. Install it (winget install gstreamerproject.gstreamer) or set GSTREAMER_1_0_ROOT_MSVC_X86_64." }
$env:PATH = "$gst\bin;$env:PATH"
$env:PKG_CONFIG = "$gst\bin\pkg-config.exe"
$env:PKG_CONFIG_PATH = "$gst\lib\pkgconfig"
Push-Location $PSScriptRoot
try {
  switch ($Rest[0]) {
    'build' { cargo build --release --features video }
    'test' { cargo test --release --features video @($Rest | Select-Object -Skip 1) }
    default { cargo @Rest }
  }
  exit $LASTEXITCODE
} finally { Pop-Location }
