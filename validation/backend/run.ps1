$ErrorActionPreference = "Stop"
$Root = Resolve-Path (Join-Path $PSScriptRoot "../..")
Push-Location $Root
try {
    cargo test -p reproit "backend::" --locked
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    $ClippyInstalled = rustup component list --installed | Select-String "^clippy-"
    if ($ClippyInstalled) {
        cargo clippy -p reproit --all-targets --locked -- -D warnings
        if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    }
    else {
        Write-Host "clippy component unavailable, native compile and tests passed"
        $global:LASTEXITCODE = 0
    }
}
finally {
    Pop-Location
}
