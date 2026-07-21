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
    $start = [System.Diagnostics.ProcessStartInfo]::new()
    $start.FileName = "powershell.exe"
    $start.Arguments = "-NoProfile -ExecutionPolicy Bypass -File `"$gate`""
    $start.UseShellExecute = $false
    $start.CreateNoWindow = $true
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    $child = [System.Diagnostics.Process]::new()
    $child.StartInfo = $start
    if (-not $child.Start()) { throw "Windows desktop gate did not start" }
    $stdoutTask = $child.StandardOutput.ReadToEndAsync()
    $stderrTask = $child.StandardError.ReadToEndAsync()
    # A Rust release build plus three .NET publishes can take several
    # minutes on the VM. This timeout is a deadlock guard, not a build-speed
    # assertion; keep it inside the remote harness's 30-minute ceiling.
    $deadline = [DateTime]::UtcNow.AddMinutes(25)
    while (-not $child.HasExited -and [DateTime]::UtcNow -lt $deadline) {
        Start-Sleep -Seconds 1
        $child.Refresh()
    }
    $timedOut = -not $child.HasExited
    if ($timedOut) {
        & taskkill.exe /PID $child.Id /T /F | Out-Null
        $child.WaitForExit()
        $code = 124
    }
    else {
        $child.WaitForExit()
        $code = $child.ExitCode
    }
    $stdoutText = $stdoutTask.GetAwaiter().GetResult()
    $stderrText = $stderrTask.GetAwaiter().GetResult()
    [System.IO.File]::WriteAllText($stdout, $stdoutText)
    [System.IO.File]::WriteAllText($stderr, $stderrText)
    [System.IO.File]::WriteAllText($log, $stdoutText + $stderrText)
    if ($timedOut) {
        "Windows desktop gate timed out after 25 minutes" |
            Add-Content -Encoding UTF8 $log
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
