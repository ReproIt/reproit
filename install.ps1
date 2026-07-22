# reproit installer for Windows. Fetches the reproit binary and provisions the
# web runner so `reproit fuzz https://yoursite.com` works link-and-go, with no
# env vars and no manual `npm install`. (macOS/Linux: use install.sh instead.)
#
#   $script = 'https://raw.githubusercontent.com/ReproIt/reproit/main/install.ps1'
#   powershell -ExecutionPolicy Bypass -c "irm $script | iex"
#
# Honors:
#   REPROIT_BIN_DIR   where reproit.exe lands   (default %LOCALAPPDATA%\Programs\reproit)
#   REPROIT_VERSION   tag to install, e.g. v1.0.0   (default: latest)
# Internal release gate:
#   REPROIT_RELEASE_BASE          asset base URL instead of GitHub Releases
#   REPROIT_SKIP_BROWSER_INSTALL  skip Playwright's browser download

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
# Windows PowerShell 5.1 may default to TLS 1.0; GitHub requires TLS 1.2+.
$protocol = [Net.ServicePointManager]::SecurityProtocol
[Net.ServicePointManager]::SecurityProtocol = `
    $protocol -bor [Net.SecurityProtocolType]::Tls12

$Repo = 'ReproIt/reproit'
$Architecture = if ($env:PROCESSOR_ARCHITEW6432) {
    $env:PROCESSOR_ARCHITEW6432
} else {
    $env:PROCESSOR_ARCHITECTURE
}
$Target = switch ($Architecture.ToUpperInvariant()) {
    'AMD64' { 'x86_64-pc-windows-msvc' }
    'ARM64' { 'aarch64-pc-windows-msvc' }
    default { throw "unsupported Windows architecture: $Architecture" }
}
$BinDir = if ($env:REPROIT_BIN_DIR) {
    $env:REPROIT_BIN_DIR
} else {
    Join-Path $env:LOCALAPPDATA 'Programs\reproit'
}

function Say([string]$Msg) { Write-Host $Msg }
function Fail([string]$Msg) { Write-Host "error: $Msg" -ForegroundColor Red; exit 1 }

# Download, failing loudly and distinguishing a missing release asset (HTTP 404)
# from a network/server problem.
function Fetch([string]$Url, [string]$Dest, [string]$Label) {
    try {
        Invoke-WebRequest -Uri $Url -OutFile $Dest -UseBasicParsing | Out-Null
        return $true
    } catch {
        $status = $null
        if ($_.Exception.Response) {
            try { $status = [int]$_.Exception.Response.StatusCode } catch { $status = $null }
        }
        if ($status -eq 404) {
            $message = "$Label not found (HTTP 404) at $Url`n"
            $message += "       the release may not carry this asset; "
            Fail ($message + "check https://github.com/$Repo/releases")
        } elseif ($status) {
            Fail "download failed for $Label (HTTP $status) from $Url"
        } else {
            Fail "network error downloading $Label from $Url ($($_.Exception.Message))"
        }
    }
}

# Compare a file's SHA-256 against the first field of a published .sha256
# sidecar (standard "<hex>  <name>" shasum/sha256sum layout).
function VerifySha256([string]$File, [string]$SumFile, [string]$Label) {
    $want = ((Get-Content -Path $SumFile -Raw).Trim() -split '\s+')[0].ToLower()
    if (-not $want) { Fail "empty checksum file for $Label" }
    $got = (Get-FileHash -Algorithm SHA256 -Path $File).Hash.ToLower()
    if ($got -ne $want) {
        Fail "checksum mismatch for $Label (expected $want, got $got); refusing to install"
    }
    Say "  verified sha256: $Label"
}

if (-not [Environment]::Is64BitOperatingSystem) {
    Fail "reproit ships 64-bit Windows binaries only (try: cargo install reproit)"
}

# --- resolve the release tag -------------------------------------------------
$Tag = $env:REPROIT_VERSION
if (-not $Tag) {
    if ($env:REPROIT_RELEASE_BASE) {
        Fail 'REPROIT_VERSION is required with REPROIT_RELEASE_BASE'
    }
    Say 'resolving the latest release...'
    try {
        $latestUrl = "https://api.github.com/repos/$Repo/releases/latest"
        $Tag = (Invoke-RestMethod -Uri $latestUrl -UseBasicParsing).tag_name
    } catch {
        $message = 'could not reach the GitHub API to resolve the latest release '
        $message += '(network problem or rate limit); retry, or pin a tag with '
        Fail ($message + "`$env:REPROIT_VERSION = 'vX.Y.Z'")
    }
    if (-not $Tag) { Fail 'could not resolve the latest release tag from the GitHub API response' }
}
Say "installing reproit $Tag ($Target)"

$Dl = if ($env:REPROIT_RELEASE_BASE) {
    $env:REPROIT_RELEASE_BASE.TrimEnd('/')
} else {
    "https://github.com/$Repo/releases/download/$Tag"
}
$randomName = "reproit-install-" + [System.IO.Path]::GetRandomFileName()
$Tmp = Join-Path ([System.IO.Path]::GetTempPath()) $randomName
New-Item -ItemType Directory -Path $Tmp -Force | Out-Null

try {
    # --- the binary ----------------------------------------------------------
    $BinAsset = "reproit-$Tag-$Target.zip"
    $BinZip = Join-Path $Tmp 'bin.zip'
    Say "  downloading $BinAsset"
    Fetch "$Dl/$BinAsset" $BinZip $BinAsset | Out-Null
    $BinSum = Join-Path $Tmp 'bin.zip.sha256'
    Fetch "$Dl/$BinAsset.sha256" $BinSum "$BinAsset.sha256" | Out-Null
    VerifySha256 $BinZip $BinSum $BinAsset
    New-Item -ItemType Directory -Path $BinDir -Force | Out-Null
    Expand-Archive -Path $BinZip -DestinationPath $BinDir -Force
    $Installed = Join-Path $BinDir 'reproit.exe'
    $InstalledVersion = (& $Installed --version) -join "`n"
    $ExpectedVersion = 'reproit ' + $Tag.TrimStart('v')
    if ($InstalledVersion.Trim() -ne $ExpectedVersion) {
        Fail "installed binary reported '$InstalledVersion', expected '$ExpectedVersion'"
    }
    Say "  installed -> $(Join-Path $BinDir 'reproit.exe')"

    # --- the web runner bundle (runner + node_modules), extracted flat -------
    # Same path the binary self-heals into (config::web_runner_data_dir), so a
    # scripted install and a runtime self-heal converge on one location.
    $WebDir = Join-Path $env:LOCALAPPDATA 'reproit\web'
    $tar = Get-Command tar.exe -ErrorAction SilentlyContinue
    if ($tar) {
        $RunnerAsset = 'reproit-web-runner.tar.gz'
        $WebTar = Join-Path $Tmp 'web.tar.gz'
        Say '  downloading web runner'
        Fetch "$Dl/$RunnerAsset" $WebTar $RunnerAsset | Out-Null
        $WebSum = Join-Path $Tmp 'web.tar.gz.sha256'
        Fetch "$Dl/$RunnerAsset.sha256" $WebSum "$RunnerAsset.sha256" | Out-Null
        VerifySha256 $WebTar $WebSum $RunnerAsset
        New-Item -ItemType Directory -Path $WebDir -Force | Out-Null
        & $tar.Source -xzf $WebTar -C $WebDir
        if ($LASTEXITCODE -ne 0) { Fail 'could not extract reproit-web-runner.tar.gz' }
        Say "  installed web runner -> $WebDir"
    } else {
        Say '  note: tar.exe was not found (ships with Windows 10 1803+); skipping the'
        Say '        web runner download. reproit provisions it itself on first web run.'
    }

    # --- one-time headless browser fetch (skipped if Node is absent) ---------
    $node = Get-Command node -ErrorAction SilentlyContinue
    $cli = Join-Path $WebDir 'node_modules\playwright\cli.js'
    if ($env:REPROIT_SKIP_BROWSER_INSTALL) {
        Say '  skipping browser fetch for release validation'
    } elseif ($node -and (Test-Path $cli)) {
        Say '  fetching the headless browser (chromium, one-time)...'
        & $node.Source $cli install chromium
        if ($LASTEXITCODE -ne 0) { Say '  (browser fetch failed; it will retry on first fuzz)' }
    } elseif (-not $node) {
        Say "  note: Node.js (18+) was not found. reproit's web fuzzer needs it;"
        Say '        install Node, then the browser fetch runs on your first fuzz.'
    }
} finally {
    Remove-Item -Path $Tmp -Recurse -Force -ErrorAction SilentlyContinue
}

# --- PATH hint ---------------------------------------------------------------
$onPath = ($env:Path -split ';') -contains $BinDir
if (-not $onPath) {
    Say ''
    Say "Add $BinDir to your PATH (run once, then open a new terminal):"
    $pathCommand = "  [Environment]::SetEnvironmentVariable('Path', `"$BinDir;`" +"
    $pathCommand += " [Environment]::GetEnvironmentVariable('Path', 'User'), 'User')"
    Say $pathCommand
}

Say ''
Say 'Done. Try:  reproit scan https://yoursite.com'
