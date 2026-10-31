$ErrorActionPreference = "Stop"
$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$RepoRoot = Split-Path -Parent $ScriptDir
if (-not $env:PASEO_HOME) { $env:PASEO_HOME = Join-Path $RepoRoot ".dev\paseo-home"; $env:PASEO_DEV_MANAGED_HOME = "1" }
if (-not $env:PASEO_LISTEN) { $env:PASEO_LISTEN = "127.0.0.1:6768" }
$env:PASEO_NODE_ENV = "development"
Push-Location $RepoRoot
try {
    & node (Join-Path $ScriptDir "dev-native-daemon.mjs") --watch @args
    exit $LASTEXITCODE
} finally {
    Pop-Location
}
