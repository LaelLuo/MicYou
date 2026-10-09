$ErrorActionPreference = 'Stop'

# Keep the portable fork's configuration and mode lock together. The distinct
# Tauri identifier isolates its WebView preferences from the upstream app.
$duplexExe = Join-Path $PSScriptRoot 'micyou.exe'
if (-not (Test-Path -LiteralPath $duplexExe -PathType Leaf)) {
    throw 'micyou.exe is missing beside this launcher.'
}
$duplexConfig = Join-Path $PSScriptRoot 'data'
New-Item -ItemType Directory -Path $duplexConfig -Force | Out-Null
$duplexPrefs = Join-Path $duplexConfig 'server.json'
if (-not (Test-Path -LiteralPath $duplexPrefs)) {
    $duplexInitialPrefs = '{"port":18554,"webPort":18443,"mode":"wifi","autoBind":true,"bindAddress":"0.0.0.0"}'
    [IO.File]::WriteAllText($duplexPrefs, $duplexInitialPrefs, [Text.UTF8Encoding]::new($false))
}

$duplexPreviousConfig = $env:MICYOU_CONFIG_DIR
try {
    $env:MICYOU_CONFIG_DIR = $duplexConfig
    Start-Process -FilePath $duplexExe -WorkingDirectory $PSScriptRoot -WindowStyle Hidden | Out-Null
} finally {
    if ($null -eq $duplexPreviousConfig) {
        Remove-Item Env:MICYOU_CONFIG_DIR -ErrorAction SilentlyContinue
    } else {
        $env:MICYOU_CONFIG_DIR = $duplexPreviousConfig
    }
}
