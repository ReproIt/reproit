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
Add-Type @"
using System;
using System.Runtime.InteropServices;
public static class ReproItInput {
    [DllImport("user32.dll")]
    public static extern void keybd_event(byte vk, byte scan, uint flags, UIntPtr extra);
    [DllImport("user32.dll")]
    public static extern bool SetForegroundWindow(IntPtr window);
}
"@

# F1 is the only modifier-free navigating shortcut the MainView handler owns, so
# holding it separates the KeyDown edge from the KeyUp edge without ever leaving
# the target's own automation tree. F7 is bound to nothing, so it is the
# neighbouring legal key: the same routed-event path must stay inert for it.
$TRIGGER_KEY = if ($Role.EndsWith("-control")) { 0x76 } else { 0x70 }
$TRIGGER_NAME = if ($Role.EndsWith("-control")) { "F7" } else { "F1" }
$KEY_UP = 2

# UniGetUI stores each setting as a flag file named after the key. Writing the
# manager switches before launch keeps winget, scoop and the rest from ever
# being spawned, which is the containment this campaign promised.
# NTFS is case-insensitive, so this list must be unique case-insensitively too:
# two spellings of the same flag make the second New-Item fail.
$SETTING_FLAGS = @(
    "AlreadyWarnedAboutAdmin", "DisableApi", "DisableAutoCheckforUpdates",
    "DisableAutoUpdateWingetUI", "DisableCargo", "DisableChocolatey",
    "DisableDotnet", "DisableGOG", "DisableIconsAndScreenshots",
    "DisableNpm", "DisablePip", "DisablePowerShell", "DisablePowerShell7",
    "DisableScoop", "DisableSteam", "DisableTelemetry", "DisableUbisoft",
    "DisableUpdatesNotifications", "DisableVcpkg", "DisableWinget",
    "ShownTelemetryBanner", "TransferredOldSettings"
)

$applicationDirectory = (Resolve-Path -LiteralPath $ApplicationDirectory).Path
$applicationPath = Join-Path $applicationDirectory "UniGetUI.exe"
$assemblyPath = Join-Path $applicationDirectory "UniGetUI.dll"
$settingsPath = Join-Path $env:LOCALAPPDATA "UniGetUI"
$inboundRule = "reproit-unigetui-inbound"
$outboundRule = "reproit-unigetui-outbound"
$startedAt = [DateTimeOffset]::UtcNow
$stopwatch = [Diagnostics.Stopwatch]::StartNew()
$keyHeld = $false
$failure = $null

$result = [ordered]@{
    schemaVersion = 1
    campaign = "unigetui-mainview-keyup-3298"
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
        settingsAbsentBeforeRun = $null
        settingFlagsWritten = $null
        firewallRulesApplied = $null
        dependencyDialogDismissed = $null
        packageManagerProcessNames = @()
        stoppedOwnedProcessIds = @()
        remainingOwnedProcessCount = $null
        settingsRemoved = $null
        firewallRulesRemoved = $null
    }
    readiness = [ordered]@{
        processId = $null
        processSessionId = $null
        processPath = $null
        focusedAutomationId = $null
        elapsedMilliseconds = $null
    }
    observation = [ordered]@{
        triggerKey = $TRIGGER_NAME
        minimizedTrigger =
            "focus the search box, hold $TRIGGER_NAME without releasing, observe, release, observe"
        neighboringControl = $Role.EndsWith("-control")
        pageBeforeTrigger = $null
        pageWhileKeyHeld = $null
        pageAfterKeyReleased = $null
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

function Find-Element {
    param(
        [Parameter(Mandatory = $true)] [int] $ProcessId,
        [Parameter(Mandatory = $true)] [string] $AutomationId,
        [Parameter(Mandatory = $true)] [int] $Seconds
    )

    $deadline = [DateTimeOffset]::UtcNow.AddSeconds($Seconds)
    $processCondition = [Windows.Automation.PropertyCondition]::new(
        [Windows.Automation.AutomationElement]::ProcessIdProperty,
        $ProcessId
    )
    $idCondition = [Windows.Automation.PropertyCondition]::new(
        [Windows.Automation.AutomationElement]::AutomationIdProperty,
        $AutomationId
    )
    $condition = [Windows.Automation.AndCondition]::new($processCondition, $idCondition)
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

function Get-PageLabel {
    param(
        [Parameter(Mandatory = $true)] [int] $ProcessId
    )

    # The Discover page keeps a MainTitle text block. The Help page replaces it
    # with a browser pane, so the destination has a positive marker of its own
    # rather than only the absence of the previous page.
    $title = Find-Element -ProcessId $ProcessId -AutomationId "MainTitle" -Seconds 2
    if ($null -ne $title) {
        return $title.Current.Name
    }
    # Do not name this $home: PowerShell owns that automatic variable and
    # assigning to it throws instead of observing anything.
    $webView = Find-Element -ProcessId $ProcessId -AutomationId "WebView" -Seconds 2
    $homeButton = Find-Element -ProcessId $ProcessId -AutomationId "HomeButton" -Seconds 2
    if ($null -ne $webView -and $null -ne $homeButton) {
        return "help-browser"
    }
    return "unknown"
}

try {
    foreach ($required in @($applicationPath, $assemblyPath)) {
        if (-not (Test-Path -LiteralPath $required -PathType Leaf)) {
            throw "Required UniGetUI file does not exist: $required"
        }
    }
    $result.exactIdentity.executableSha256 =
        (Get-FileHash -LiteralPath $applicationPath -Algorithm SHA256).Hash
    $result.exactIdentity.assemblySha256 =
        (Get-FileHash -LiteralPath $assemblyPath -Algorithm SHA256).Hash
    if ($result.exactIdentity.assemblySha256 -ne $ExpectedAssemblySha256.ToUpperInvariant()) {
        throw "UniGetUI.dll does not match the expected revision hash."
    }
    $result.assertions += "assembly-hash-matches-exact-revision"

    $result.containment.initialProcessCount = @(Get-OwnedProcesses).Count
    if ($result.containment.initialProcessCount -ne 0) {
        throw "Refusing to start while an owned UniGetUI process is already running."
    }
    $result.assertions += "no-preexisting-owned-process"

    Remove-Item -LiteralPath $settingsPath -Recurse -Force -ErrorAction SilentlyContinue
    $result.containment.settingsAbsentBeforeRun = -not (Test-Path -LiteralPath $settingsPath)
    if (-not $result.containment.settingsAbsentBeforeRun) {
        throw "The UniGetUI settings directory survived the pre-run reset."
    }
    New-Item -ItemType Directory -Path $settingsPath | Out-Null
    foreach ($flag in $SETTING_FLAGS) {
        New-Item -ItemType File -Path (Join-Path $settingsPath $flag) | Out-Null
    }
    $result.containment.settingFlagsWritten = $SETTING_FLAGS.Count
    $result.assertions += "fresh-settings-with-package-managers-disabled"

    Remove-NetFirewallRule -DisplayName $inboundRule -ErrorAction SilentlyContinue
    Remove-NetFirewallRule -DisplayName $outboundRule -ErrorAction SilentlyContinue
    New-NetFirewallRule -DisplayName $inboundRule -Direction Inbound `
        -Program $applicationPath -Action Block -Profile Any | Out-Null
    New-NetFirewallRule -DisplayName $outboundRule -Direction Outbound `
        -Program $applicationPath -Action Block -Profile Any | Out-Null
    $result.containment.firewallRulesApplied = $true
    $result.assertions += "program-scoped-network-block"

    $started = Start-Process -FilePath $applicationPath `
        -WorkingDirectory $applicationDirectory -PassThru
    $applicationProcessId = $started.Id
    $result.readiness.processId = $applicationProcessId

    $owned = @(Get-OwnedProcesses | Where-Object { $_.ProcessId -eq $applicationProcessId })
    if ($owned.Count -ne 1) {
        throw "The launched UniGetUI process is not owned by the campaign directory."
    }
    $result.readiness.processSessionId = [int]$owned[0].SessionId
    $result.readiness.processPath = $owned[0].ExecutablePath
    if ($result.readiness.processSessionId -ne $result.environment.sessionId) {
        throw "UniGetUI did not start in the harness desktop session."
    }
    $result.assertions += "owned-process-in-interactive-session"

    # Bounded setup, recorded rather than hidden: the administrator notice and
    # the Cargo dependency notice are modal content dialogs that stand between
    # launch and the page under test. Dismissing them installs nothing.
    $acknowledge = Find-Element -ProcessId $applicationProcessId `
        -AutomationId "PrimaryButton" -Seconds 240
    if ($null -eq $acknowledge) {
        throw "The administrator notice never entered the UI Automation tree."
    }
    $acknowledge.GetCurrentPattern(
        [Windows.Automation.InvokePattern]::Pattern
    ).Invoke()
    Start-Sleep -Seconds 3
    $dependency = Find-Element -ProcessId $applicationProcessId `
        -AutomationId "SecondaryButton" -Seconds 30
    $result.containment.dependencyDialogDismissed = $null -ne $dependency
    if ($null -ne $dependency) {
        $dependency.GetCurrentPattern(
            [Windows.Automation.InvokePattern]::Pattern
        ).Invoke()
        Start-Sleep -Seconds 3
    }
    $result.assertions += "modal-setup-dialogs-dismissed"

    $processCondition = [Windows.Automation.PropertyCondition]::new(
        [Windows.Automation.AutomationElement]::ProcessIdProperty,
        $applicationProcessId
    )
    $window = [Windows.Automation.AutomationElement]::RootElement.FindFirst(
        [Windows.Automation.TreeScope]::Children,
        $processCondition
    )
    if ($null -eq $window) {
        throw "UniGetUI never presented a top-level window to UI Automation."
    }
    [ReproItInput]::SetForegroundWindow([IntPtr]$window.Current.NativeWindowHandle) |
        Out-Null
    Start-Sleep -Seconds 1

    $query = Find-Element -ProcessId $applicationProcessId `
        -AutomationId "QueryBlock" -Seconds 60
    if ($null -eq $query) {
        throw "The search box never entered the UI Automation tree."
    }
    $query.SetFocus()
    Start-Sleep -Seconds 2
    $focused = [Windows.Automation.AutomationElement]::FocusedElement
    $result.readiness.focusedAutomationId = $focused.Current.AutomationId
    if ($result.readiness.focusedAutomationId -ne "QueryBlock") {
        throw "Keyboard focus did not land inside MainView."
    }
    $result.readiness.elapsedMilliseconds = $stopwatch.ElapsedMilliseconds
    $result.assertions += "focus-inside-mainview"

    $result.observation.pageBeforeTrigger = Get-PageLabel -ProcessId $applicationProcessId
    if ($result.observation.pageBeforeTrigger -ne "Discover Packages") {
        throw "The run did not start on the Discover page."
    }

    [ReproItInput]::keybd_event([byte]$TRIGGER_KEY, 0, 0, [UIntPtr]::Zero)
    $keyHeld = $true
    Start-Sleep -Seconds 5
    $result.observation.pageWhileKeyHeld = Get-PageLabel -ProcessId $applicationProcessId

    [ReproItInput]::keybd_event([byte]$TRIGGER_KEY, 0, $KEY_UP, [UIntPtr]::Zero)
    $keyHeld = $false
    Start-Sleep -Seconds 6
    $result.observation.pageAfterKeyReleased = Get-PageLabel -ProcessId $applicationProcessId
    $result.observation.observationReached = $true
    $result.assertions += "both-key-edges-observed"

    $held = $result.observation.pageWhileKeyHeld
    $released = $result.observation.pageAfterKeyReleased
    switch ($Role) {
        "affected" {
            if ($held -ne "Discover Packages" -or $released -ne "help-browser") {
                throw "Affected run expected held=Discover Packages, released=help-browser."
            }
            $result.observation.identity =
                "navigation-shortcut-fires-on-key-release-not-key-press"
        }
        "fixed" {
            if ($held -ne "help-browser" -or $released -ne "help-browser") {
                throw "Fixed control expected the navigation to land while the key was held."
            }
        }
        default {
            if ($held -ne "Discover Packages" -or $released -ne "Discover Packages") {
                throw "Neighbouring legal key must leave the page on Discover Packages."
            }
        }
    }
    $result.assertions += "role-verdict-matches-observation"

    $result.containment.packageManagerProcessNames = @(
        Get-Process -Name "winget", "scoop", "choco", "npm", "cargo", "pip" `
            -ErrorAction SilentlyContinue |
            Select-Object -ExpandProperty Name -Unique
    )
    if ($result.containment.packageManagerProcessNames.Count -ne 0) {
        throw "A package manager process was spawned during the run."
    }
    $result.assertions += "no-package-manager-process-spawned"
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
    if ($keyHeld) {
        [ReproItInput]::keybd_event([byte]$TRIGGER_KEY, 0, $KEY_UP, [UIntPtr]::Zero)
    }
    $result.containment.stoppedOwnedProcessIds = @(Stop-OwnedProcesses)
    Start-Sleep -Milliseconds 1500
    $result.containment.remainingOwnedProcessCount = @(Get-OwnedProcesses).Count

    Remove-Item -LiteralPath $settingsPath -Recurse -Force -ErrorAction SilentlyContinue
    $result.containment.settingsRemoved = -not (Test-Path -LiteralPath $settingsPath)

    Remove-NetFirewallRule -DisplayName $inboundRule -ErrorAction SilentlyContinue
    Remove-NetFirewallRule -DisplayName $outboundRule -ErrorAction SilentlyContinue
    $result.containment.firewallRulesRemoved = 0 -eq @(
        Get-NetFirewallRule -DisplayName "reproit-unigetui-*" -ErrorAction SilentlyContinue
    ).Count

    if ($result.containment.remainingOwnedProcessCount -ne 0 -or
        -not $result.containment.settingsRemoved -or
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
