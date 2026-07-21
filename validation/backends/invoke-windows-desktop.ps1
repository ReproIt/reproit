$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"
Set-StrictMode -Version Latest

$root = (Resolve-Path (Join-Path $PSScriptRoot "../..")).Path
$state = Join-Path $env:TEMP "reproit-windows-backends"
$result = Join-Path $state "interactive.exit"
$log = Join-Path $state "interactive.log"
$script = Join-Path $root "validation/backends/run-windows-desktop-interactive.ps1"
$taskName = "ReproitBackendGate"

New-Item -ItemType Directory -Force -Path $state | Out-Null
Remove-Item $result,$log -Force -ErrorAction SilentlyContinue
$taskArgs = "-NoProfile -ExecutionPolicy Bypass -File `"$script`""
$action = New-ScheduledTaskAction -Execute "powershell.exe" -Argument $taskArgs
$user = [System.Security.Principal.WindowsIdentity]::GetCurrent().Name
$principal = New-ScheduledTaskPrincipal `
    -UserId $user -LogonType Interactive -RunLevel Highest

try {
    Register-ScheduledTask `
        -TaskName $taskName -Action $action -Principal $principal -Force | Out-Null
    Start-ScheduledTask -TaskName $taskName
    $deadline = [DateTime]::UtcNow.AddMinutes(30)
    while (-not (Test-Path $result) -and [DateTime]::UtcNow -lt $deadline) {
        Start-Sleep -Seconds 2
    }
    if (-not (Test-Path $result)) {
        throw "interactive Windows backend gate timed out"
    }
    Get-Content $log
    $exitCode = [int](Get-Content -Raw $result)
    if ($exitCode -ne 0) {
        throw "interactive Windows backend gate failed"
    }
}
finally {
    Stop-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue
    Unregister-ScheduledTask `
        -TaskName $taskName -Confirm:$false -ErrorAction SilentlyContinue
}
