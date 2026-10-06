# Builds and runs the spike with GStreamer's paths set for this process only.
# Usage: .\run.ps1 [test|build|run] [-- spike args]
#   .\run.ps1 run -- --seconds 60 --source webcam
#   .\run.ps1 run -Cores 2 -- --seconds 60      (simulate a weak laptop: pin to 2 cores)
[CmdletBinding(PositionalBinding = $false)]
param(
  [Parameter(Position = 0)] [ValidateSet('test', 'build', 'run')] [string]$Command = 'run',
  [int]$Cores = 0,
  [Parameter(ValueFromRemainingArguments = $true)] [string[]]$Rest
)
# Not 'Stop': cargo writes progress to stderr, which PowerShell 5.1 would treat as fatal.

$gst = $env:GSTREAMER_1_0_ROOT_MSVC_X86_64
if (-not $gst) { $gst = Join-Path $env:LOCALAPPDATA 'Programs\gstreamer\1.0\msvc_x86_64' }
if (-not (Test-Path "$gst\bin\gst-launch-1.0.exe")) { throw "GStreamer not found at $gst. Install it or set GSTREAMER_1_0_ROOT_MSVC_X86_64." }
$env:PATH = "$gst\bin;$env:PATH"
$env:PKG_CONFIG = "$gst\bin\pkg-config.exe"
$env:PKG_CONFIG_PATH = "$gst\lib\pkgconfig"

# Optional local stream key file (git-ignored). Never commit or print it.
$keyFile = Join-Path $PSScriptRoot '.stream-key'
if (-not $env:JIVVY_YT_STREAM_KEY -and (Test-Path $keyFile)) {
  $env:JIVVY_YT_STREAM_KEY = (Get-Content $keyFile -Raw).Trim()
}

Push-Location $PSScriptRoot
try {
  $args2 = @($Rest | Where-Object { $_ -ne '--' })
  switch ($Command) {
    'test' { cargo test --release }
    'build' { cargo build --release }
    'run' {
      cargo build --release
      if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
      $exe = Join-Path $PSScriptRoot 'target\release\lyrics-stream.exe'
      if ($Cores -gt 0) {
        $p = Start-Process -FilePath $exe -ArgumentList $args2 -NoNewWindow -PassThru
        $p.ProcessorAffinity = [IntPtr]([int64][math]::Pow(2, $Cores) - 1)
        $p.WaitForExit()
        exit $p.ExitCode
      }
      & $exe @args2
    }
  }
  exit $LASTEXITCODE
} finally { Pop-Location }
