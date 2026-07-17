$ErrorActionPreference = "Continue"
$root = (Resolve-Path (Join-Path $PSScriptRoot "../..")).Path
$state = Join-Path $env:TEMP "reproit-windows-backends"
$log = Join-Path $state "interactive.log"
$stdout = Join-Path $state "interactive.stdout.log"
$stderr = Join-Path $state "interactive.stderr.log"
$result = Join-Path $state "interactive.exit"

New-Item -ItemType Directory -Force -Path $state | Out-Null
Remove-Item $log, $stdout, $stderr, $result -Force -ErrorAction SilentlyContinue

try {
    $gate = Join-Path $PSScriptRoot "run-windows-desktop.ps1"
    $child = Start-Process -FilePath "powershell.exe" `
        -ArgumentList @("-NoProfile", "-ExecutionPolicy", "Bypass", "-File", $gate) `
        -RedirectStandardOutput $stdout -RedirectStandardError $stderr `
        -PassThru
    # A clean Rust release build plus three .NET publishes can take several
    # minutes on the VM. This timeout is a deadlock guard, not a build-speed
    # assertion; keep it inside the remote harness's 15-minute ceiling.
    $deadline = [DateTime]::UtcNow.AddMinutes(12)
    while (-not $child.HasExited -and [DateTime]::UtcNow -lt $deadline) {
        Start-Sleep -Seconds 1
        $child.Refresh()
    }
    $timedOut = -not $child.HasExited
    if ($timedOut) {
        Stop-Process -Id $child.Id -Force -ErrorAction SilentlyContinue
        $code = 124
    }
    else {
        $child.WaitForExit()
        $child.Refresh()
        $code = $child.ExitCode
    }
    $stdoutText = if (Test-Path $stdout) { Get-Content -Raw $stdout } else { "" }
    $stdoutText | Set-Content -Encoding UTF8 $log
    if (Test-Path $stderr) { Get-Content -Raw $stderr | Add-Content -Encoding UTF8 $log }
    if ($timedOut) {
        "Windows desktop gate timed out after 12 minutes" |
            Add-Content -Encoding UTF8 $log
    }
    if (-not $timedOut -and $null -eq $code) {
        $terminalMarker = "Windows DesktopUia backend passed WPF, Avalonia, and WinUI"
        if ($stdoutText.Contains($terminalMarker)) {
            $code = 0
            "WARNING: child exit code was unavailable; accepting the exact terminal matrix marker" |
                Add-Content -Encoding UTF8 $log
        }
        else {
            $code = 1
        }
    }
}
catch {
    $_ | Out-String | Add-Content -Encoding UTF8 $log
    $code = 1
}
finally {
    [System.IO.File]::WriteAllText($result, [string]$code)
}

exit $code
