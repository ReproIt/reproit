[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateScript({ Test-Path -LiteralPath $_ -PathType Container })]
    [string] $ApplicationDirectory,

    [Parameter(Mandatory = $true)]
    [ValidatePattern("^[0-9a-f]{40}$")]
    [string] $ExpectedRevision,

    [Parameter(Mandatory = $true)]
    [ValidatePattern("^[0-9A-Fa-f]{64}$")]
    [string] $ExpectedAssemblySha256,

    [Parameter(Mandatory = $true)]
    [ValidateSet("affected", "fixed", "affected-control", "fixed-control")]
    [string] $Role,

    [Parameter(Mandatory = $true)]
    [ValidateRange(1, 3)]
    [int] $Run,

    [Parameter(Mandatory = $true)]
    [string] $OutputPath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"

Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes

# Windows parks a minimized window at this sentinel coordinate. The defect is
# that the affected revision persists it on exit and restores it on the next
# launch, so the application comes back permanently offscreen.
$MINIMIZED_SENTINEL = -30000

$applicationDirectory = (Resolve-Path -LiteralPath $ApplicationDirectory).Path
$applicationPath = Join-Path $applicationDirectory "DLSS Swapper.exe"
$assemblyPath = Join-Path $applicationDirectory "DLSS Swapper.dll"
# The portable configuration keeps every byte of its own state here, so the
# reset is a directory removal and never touches anything outside the run root.
$storedDataPath = Join-Path $applicationDirectory "StoredData"
$inboundRule = "reproit-dlss-inbound"
$outboundRule = "reproit-dlss-outbound"
$startedAt = [DateTimeOffset]::UtcNow
$stopwatch = [Diagnostics.Stopwatch]::StartNew()
$failure = $null

$result = [ordered]@{
    schemaVersion = 1
    campaign = "dlss-swapper-minimized-window-position-829"
    target = "windows-winui"
    role = $Role
    run = $Run
    status = "running"
    startedAt = $startedAt.ToString("O")
    finishedAt = $null
    exactIdentity = [ordered]@{
        revision = $ExpectedRevision
        applicationPath = $applicationPath
        executableSha256 = $null
        assemblySha256 = $null
    }
    environment = [ordered]@{
        os = [Environment]::OSVersion.VersionString
        architecture =
            [Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString()
        sessionId = (Get-Process -Id $PID).SessionId
        automation = "Windows UI Automation"
        appModel = "unpackaged, WindowsPackageType None, WindowsAppSDKSelfContained"
        networkPolicy =
            "program-scoped Windows Firewall inbound and outbound block rules"
    }
    containment = [ordered]@{
        initialProcessCount = $null
        storedDataAbsentBeforeRun = $null
        firewallRulesApplied = $null
        firstRunNoticesDismissed = $null
        firstLaunchExitedCleanly = $null
        stoppedOwnedProcessIds = @()
        remainingOwnedProcessCount = $null
        storedDataRemoved = $null
        firewallRulesRemoved = $null
    }
    readiness = [ordered]@{
        firstProcessId = $null
        firstProcessSessionId = $null
        secondProcessId = $null
        elapsedMilliseconds = $null
    }
    observation = [ordered]@{
        minimizedTrigger = if ($Role.EndsWith("-control")) {
            "close the window through the window pattern from its normal state, then relaunch"
        }
        else {
            "minimize then close through the window pattern, then relaunch"
        }
        neighboringControl = $Role.EndsWith("-control")
        firstLaunchRectangle = $null
        firstLaunchVisualState = $null
        secondLaunchRectangle = $null
        secondLaunchVisualState = $null
        secondLaunchOffscreen = $null
        identity = $null
        observationReached = $false
    }
    assertions = @()
    failure = $null
}

function Get-OwnedProcesses {
    Get-CimInstance Win32_Process |
        Where-Object {
            $null -ne $_.ExecutablePath -and
            $_.ExecutablePath.StartsWith(
                $applicationDirectory,
                [StringComparison]::OrdinalIgnoreCase
            )
        }
}

function Stop-OwnedProcesses {
    $stopped = [System.Collections.Generic.List[int]]::new()
    foreach ($owned in @(Get-OwnedProcesses)) {
        $processId = [int]$owned.ProcessId
        Stop-Process -Id $processId -Force -ErrorAction SilentlyContinue
        $stopped.Add($processId)
    }
    return @($stopped)
}

function Wait-ForWindow {
    param(
        [Parameter(Mandatory = $true)] [int] $ProcessId,
        [Parameter(Mandatory = $true)] [int] $Seconds
    )

    $condition = [Windows.Automation.PropertyCondition]::new(
        [Windows.Automation.AutomationElement]::ProcessIdProperty,
        $ProcessId
    )
    $deadline = [DateTimeOffset]::UtcNow.AddSeconds($Seconds)
    while ([DateTimeOffset]::UtcNow -lt $deadline) {
        $window = [Windows.Automation.AutomationElement]::RootElement.FindFirst(
            [Windows.Automation.TreeScope]::Children,
            $condition
        )
        if ($null -ne $window) {
            return $window
        }
        Start-Sleep -Milliseconds 500
    }
    return $null
}

function Find-CloseButton {
    param(
        [Parameter(Mandatory = $true)] [int] $ProcessId,
        [Parameter(Mandatory = $true)] [int] $Seconds
    )

    $processCondition = [Windows.Automation.PropertyCondition]::new(
        [Windows.Automation.AutomationElement]::ProcessIdProperty,
        $ProcessId
    )
    $idCondition = [Windows.Automation.PropertyCondition]::new(
        [Windows.Automation.AutomationElement]::AutomationIdProperty,
        "CloseButton"
    )
    $condition = [Windows.Automation.AndCondition]::new($processCondition, $idCondition)
    $deadline = [DateTimeOffset]::UtcNow.AddSeconds($Seconds)
    while ([DateTimeOffset]::UtcNow -lt $deadline) {
        $element = [Windows.Automation.AutomationElement]::RootElement.FindFirst(
            [Windows.Automation.TreeScope]::Descendants,
            $condition
        )
        if ($null -ne $element) {
            return $element
        }
        Start-Sleep -Milliseconds 500
    }
    return $null
}

function Format-Rectangle {
    param([Parameter(Mandatory = $true)] $Rectangle)

    return [ordered]@{
        x = [int]$Rectangle.X
        y = [int]$Rectangle.Y
        width = [int]$Rectangle.Width
        height = [int]$Rectangle.Height
    }
}

try {
    foreach ($required in @($applicationPath, $assemblyPath)) {
        if (-not (Test-Path -LiteralPath $required -PathType Leaf)) {
            throw "Required DLSS Swapper file does not exist: $required"
        }
    }
    $result.exactIdentity.executableSha256 =
        (Get-FileHash -LiteralPath $applicationPath -Algorithm SHA256).Hash
    $result.exactIdentity.assemblySha256 =
        (Get-FileHash -LiteralPath $assemblyPath -Algorithm SHA256).Hash
    if ($result.exactIdentity.assemblySha256 -ne $ExpectedAssemblySha256.ToUpperInvariant()) {
        throw "DLSS Swapper.dll does not match the expected revision hash."
    }
    $result.assertions += "assembly-hash-matches-exact-revision"

    $result.containment.initialProcessCount = @(Get-OwnedProcesses).Count
    if ($result.containment.initialProcessCount -ne 0) {
        throw "Refusing to start while an owned DLSS Swapper process is already running."
    }
    $result.assertions += "no-preexisting-owned-process"

    Remove-Item -LiteralPath $storedDataPath -Recurse -Force -ErrorAction SilentlyContinue
    $result.containment.storedDataAbsentBeforeRun =
        -not (Test-Path -LiteralPath $storedDataPath)
    if (-not $result.containment.storedDataAbsentBeforeRun) {
        throw "The portable StoredData directory survived the pre-run reset."
    }
    $result.assertions += "fresh-portable-stored-data"

    Remove-NetFirewallRule -DisplayName $inboundRule -ErrorAction SilentlyContinue
    Remove-NetFirewallRule -DisplayName $outboundRule -ErrorAction SilentlyContinue
    New-NetFirewallRule -DisplayName $inboundRule -Direction Inbound `
        -Program $applicationPath -Action Block -Profile Any | Out-Null
    New-NetFirewallRule -DisplayName $outboundRule -Direction Outbound `
        -Program $applicationPath -Action Block -Profile Any | Out-Null
    $result.containment.firewallRulesApplied = $true
    $result.assertions += "program-scoped-network-block"

    $first = Start-Process -FilePath $applicationPath `
        -WorkingDirectory $applicationDirectory -PassThru
    $result.readiness.firstProcessId = $first.Id
    $firstWindow = Wait-ForWindow -ProcessId $first.Id -Seconds 180
    if ($null -eq $firstWindow) {
        throw "DLSS Swapper never presented a window on the first launch."
    }
    $owned = @(Get-OwnedProcesses | Where-Object { $_.ProcessId -eq $first.Id })
    if ($owned.Count -ne 1) {
        throw "The launched DLSS Swapper process is not owned by the campaign directory."
    }
    $result.readiness.firstProcessSessionId = [int]$owned[0].SessionId
    if ($result.readiness.firstProcessSessionId -ne $result.environment.sessionId) {
        throw "DLSS Swapper did not start in the harness desktop session."
    }
    $result.assertions += "owned-process-in-interactive-session"

    # Bounded setup, recorded rather than hidden: the first launch of a fresh
    # portable profile shows a multiplayer advisory that has to be acknowledged
    # before the window pattern is usable. Acknowledging it changes no files.
    $dismissed = 0
    for ($attempt = 0; $attempt -lt 3; $attempt++) {
        $notice = Find-CloseButton -ProcessId $first.Id -Seconds 45
        if ($null -eq $notice) {
            break
        }
        $notice.GetCurrentPattern([Windows.Automation.InvokePattern]::Pattern).Invoke()
        $dismissed++
        Start-Sleep -Seconds 3
    }
    $result.containment.firstRunNoticesDismissed = $dismissed

    $firstWindow = Wait-ForWindow -ProcessId $first.Id -Seconds 30
    if ($null -eq $firstWindow) {
        throw "The first-launch window left the UI Automation tree during setup."
    }
    $result.observation.firstLaunchRectangle =
        Format-Rectangle -Rectangle $firstWindow.Current.BoundingRectangle
    $windowPattern = $firstWindow.GetCurrentPattern(
        [Windows.Automation.WindowPattern]::Pattern
    )
    $result.observation.firstLaunchVisualState =
        $windowPattern.Current.WindowVisualState.ToString()
    if ($result.observation.firstLaunchRectangle.x -le $MINIMIZED_SENTINEL) {
        throw "The first launch was already offscreen, so the trigger proves nothing."
    }
    $result.readiness.elapsedMilliseconds = $stopwatch.ElapsedMilliseconds
    $result.assertions += "first-launch-onscreen"

    # The trigger stays inside the target's own automation tree: minimize and
    # close both go through the UI Automation window pattern, never a raw Win32
    # call. The control closes from the normal state instead of the minimized
    # one, which is the neighbouring legal exit through the same persist path.
    if (-not $Role.EndsWith("-control")) {
        if (-not $windowPattern.Current.CanMinimize) {
            throw "The window does not advertise the minimize capability."
        }
        $windowPattern.SetWindowVisualState(
            [Windows.Automation.WindowVisualState]::Minimized
        )
        Start-Sleep -Seconds 4
        if ($windowPattern.Current.WindowVisualState -ne
            [Windows.Automation.WindowVisualState]::Minimized) {
            throw "The window did not reach the minimized visual state."
        }
        $result.assertions += "minimized-through-window-pattern"
    }
    $windowPattern.Close()

    for ($attempt = 0; $attempt -lt 30; $attempt++) {
        Start-Sleep -Seconds 1
        $first.Refresh()
        if ($first.HasExited) {
            break
        }
    }
    $first.Refresh()
    $result.containment.firstLaunchExitedCleanly = $first.HasExited
    if (-not $first.HasExited) {
        throw "The first launch did not exit after the window pattern closed it."
    }
    Start-Sleep -Seconds 3
    $result.assertions += "closed-through-window-pattern"

    $second = Start-Process -FilePath $applicationPath `
        -WorkingDirectory $applicationDirectory -PassThru
    $result.readiness.secondProcessId = $second.Id
    $secondWindow = Wait-ForWindow -ProcessId $second.Id -Seconds 180
    if ($null -eq $secondWindow) {
        throw "DLSS Swapper never presented a window on the second launch."
    }
    Start-Sleep -Seconds 8
    $secondWindow = Wait-ForWindow -ProcessId $second.Id -Seconds 30
    if ($null -eq $secondWindow) {
        throw "The second-launch window left the UI Automation tree before observation."
    }
    $rectangle = $secondWindow.Current.BoundingRectangle
    $result.observation.secondLaunchRectangle = Format-Rectangle -Rectangle $rectangle
    $result.observation.secondLaunchVisualState = $secondWindow.GetCurrentPattern(
        [Windows.Automation.WindowPattern]::Pattern
    ).Current.WindowVisualState.ToString()
    $result.observation.secondLaunchOffscreen =
        $result.observation.secondLaunchRectangle.x -le $MINIMIZED_SENTINEL -or
        $result.observation.secondLaunchRectangle.y -le $MINIMIZED_SENTINEL
    $result.observation.observationReached = $true
    $result.assertions += "relaunch-geometry-observed"

    switch ($Role) {
        "affected" {
            if (-not $result.observation.secondLaunchOffscreen) {
                throw "Affected run expected the relaunched window at the minimized sentinel."
            }
            $result.observation.identity =
                "restored-window-parks-at-minimized-sentinel-position"
        }
        "fixed" {
            if ($result.observation.secondLaunchOffscreen) {
                throw "Fixed control still restored the window offscreen."
            }
        }
        default {
            if ($result.observation.secondLaunchOffscreen) {
                throw "A normal-state close must never restore the window offscreen."
            }
        }
    }
    $result.assertions += "role-verdict-matches-observation"
    $result.status = "pass"
} catch {
    $failure = $_
    $result.status = "fail"
    $result.failure = [ordered]@{
        message = $_.Exception.Message
        category = $_.CategoryInfo.Category.ToString()
        stack = $_.ScriptStackTrace
    }
} finally {
    $result.containment.stoppedOwnedProcessIds = @(Stop-OwnedProcesses)
    Start-Sleep -Milliseconds 1500
    $result.containment.remainingOwnedProcessCount = @(Get-OwnedProcesses).Count

    Remove-Item -LiteralPath $storedDataPath -Recurse -Force -ErrorAction SilentlyContinue
    $result.containment.storedDataRemoved = -not (Test-Path -LiteralPath $storedDataPath)

    Remove-NetFirewallRule -DisplayName $inboundRule -ErrorAction SilentlyContinue
    Remove-NetFirewallRule -DisplayName $outboundRule -ErrorAction SilentlyContinue
    $result.containment.firewallRulesRemoved = 0 -eq @(
        Get-NetFirewallRule -DisplayName "reproit-dlss-*" -ErrorAction SilentlyContinue
    ).Count

    if ($result.containment.remainingOwnedProcessCount -ne 0 -or
        -not $result.containment.storedDataRemoved -or
        -not $result.containment.firewallRulesRemoved) {
        $result.status = "fail"
        if ($null -eq $result.failure) {
            $result.failure = [ordered]@{
                message = "Containment cleanup failed."
                category = "ResourceUnavailable"
                stack = $null
            }
        }
    }

    $result.finishedAt = [DateTimeOffset]::UtcNow.ToString("O")
    $outputDirectory = Split-Path -Parent $OutputPath
    if (-not [string]::IsNullOrWhiteSpace($outputDirectory)) {
        New-Item -ItemType Directory -Path $outputDirectory -Force | Out-Null
    }
    $result |
        ConvertTo-Json -Depth 10 |
        Set-Content -LiteralPath $OutputPath -Encoding utf8
}

if ($null -ne $failure) {
    throw $failure
}
if ($result.status -ne "pass") {
    throw $result.failure.message
}
