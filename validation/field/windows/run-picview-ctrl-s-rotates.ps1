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
    [string] $ExpectedApplicationSha256,

    [Parameter(Mandatory = $true)]
    [ValidateSet("affected", "fixed", "control")]
    [string] $Role,

    [Parameter(Mandatory = $true)]
    [ValidateRange(1, 3)]
    [int] $Run,

    [Parameter(Mandatory = $true)]
    [string] $OutputPath,

    [ValidateScript({ Test-Path -LiteralPath $_ -PathType Container })]
    [string] $DotnetRoot = "C:\lab\dotnet11"
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"

Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes
Add-Type -AssemblyName System.Drawing
Add-Type @"
using System;
using System.Runtime.InteropServices;

public static class ReproitPicViewInput
{
    [DllImport("user32.dll")]
    public static extern IntPtr GetForegroundWindow();

    [DllImport("user32.dll")]
    public static extern bool SetForegroundWindow(IntPtr window);

    [DllImport("user32.dll")]
    public static extern void keybd_event(
        byte virtualKey,
        byte scanCode,
        uint flags,
        UIntPtr extraInfo
    );
}
"@

$applicationDirectory = (Resolve-Path -LiteralPath $ApplicationDirectory).Path
$applicationPath = Join-Path $applicationDirectory "PicView.exe"
$applicationAssemblyPath = Join-Path $applicationDirectory "PicView.dll"
$configPath = Join-Path $env:APPDATA "Ruben2776\PicView"
$runRoot = Join-Path $env:TEMP "reproit-picview-$PID-$Role-$Run"
$imagePath = Join-Path $runRoot "picview-probe.png"
$startedAt = [DateTimeOffset]::UtcNow
$stopwatch = [Diagnostics.Stopwatch]::StartNew()
$applicationProcess = $null
$focusProcess = $null
$failure = $null
$keyUpFlag = 2
$controlVirtualKey = 0x11
$sVirtualKey = 0x53

$result = [ordered]@{
    schemaVersion = 1
    campaign = "picview-ctrl-s-rotates-342"
    target = "windows-avalonia"
    role = $Role
    run = $Run
    status = "running"
    startedAt = $startedAt.ToString("O")
    finishedAt = $null
    exactIdentity = [ordered]@{
        revision = $ExpectedRevision
        applicationPath = $applicationPath
        executableSha256 = $null
        applicationAssemblySha256 = $null
    }
    environment = [ordered]@{
        os = [Environment]::OSVersion.VersionString
        architecture =
            [Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString()
        sessionId = (Get-Process -Id $PID).SessionId
        dotnetSdk = (& (Join-Path $DotnetRoot "dotnet.exe") --version)
        automation = "Windows UI Automation"
        networkPolicy = "not isolated; the local image trigger requires no network"
    }
    containment = [ordered]@{
        initialPicViewProcessCount = $null
        configAbsentBeforeRun = $null
        runRootAbsentBeforeRun = $null
        stoppedApplicationProcess = $false
        stoppedFocusProcess = $false
        remainingApplicationProcessCount = $null
        configRemoved = $null
        runRootRemoved = $null
        controlKeyReleased = $false
    }
    readiness = [ordered]@{
        processId = $null
        processPath = $null
        processSessionId = $null
        windowNameBeforeAction = $null
        mainViewAutomationId = $null
        titleAutomationId = $null
        elapsedMilliseconds = $null
    }
    action = [ordered]@{
        kind =
            if ($Role -eq "control") {
                "bare S"
            } else {
                "hold Ctrl, deactivate, reactivate while held, press S"
            }
        appHandle = $null
        focusHelperHandle = $null
        foregroundAfterAppFocus = $null
        foregroundAfterHelperFocus = $null
        foregroundAfterReturnFocus = $null
    }
    observation = [ordered]@{
        initialFileSha256 = $null
        finalFileSha256 = $null
        fileChanged = $null
        expectedFileChanged = $Role -eq "fixed"
        windowNameAfterAction = $null
        titleNameAfterAction = $null
        renderedColorWidth = $null
        renderedColorHeight = $null
        expectedOrientation = if ($Role -eq "fixed") { "landscape" } else { "portrait" }
        observationReached = $false
        minimizedTrigger =
            if ($Role -eq "control") {
                "focus the fresh local image and press bare S once"
            } else {
                "hold Ctrl, deactivate and reactivate the image window, then press S once"
            }
        neighboringControl = $Role -eq "control"
    }
    assertions = @()
    failure = $null
}

function Get-OwnedApplicationProcesses {
    @(Get-CimInstance Win32_Process |
        Where-Object {
            $null -ne $_.ExecutablePath -and
            $_.ExecutablePath.StartsWith(
                $applicationDirectory,
                [StringComparison]::OrdinalIgnoreCase
            )
        })
}

function Wait-ForMainWindow {
    param(
        [Parameter(Mandatory = $true)]
        [Diagnostics.Process] $Process
    )

    $condition = [Windows.Automation.PropertyCondition]::new(
        [Windows.Automation.AutomationElement]::ProcessIdProperty,
        $Process.Id
    )
    $deadline = [DateTimeOffset]::UtcNow.AddSeconds(30)
    while ([DateTimeOffset]::UtcNow -lt $deadline) {
        $Process.Refresh()
        $window = [Windows.Automation.AutomationElement]::RootElement.FindFirst(
            [Windows.Automation.TreeScope]::Children,
            $condition
        )
        if ($null -ne $window -and $Process.MainWindowHandle -ne [IntPtr]::Zero) {
            return $window
        }
        Start-Sleep -Milliseconds 250
    }
    throw "PicView did not publish a main window to UI Automation."
}

function Wait-ForFocusHelper {
    param(
        [Parameter(Mandatory = $true)]
        [Diagnostics.Process] $Process
    )

    $deadline = [DateTimeOffset]::UtcNow.AddSeconds(20)
    while ([DateTimeOffset]::UtcNow -lt $deadline) {
        $Process.Refresh()
        if ($Process.MainWindowHandle -ne [IntPtr]::Zero) {
            return $Process.MainWindowHandle
        }
        Start-Sleep -Milliseconds 250
    }
    throw "The owned focus-helper window did not become ready."
}

function Get-ElementByAutomationId {
    param(
        [Parameter(Mandatory = $true)]
        [Windows.Automation.AutomationElement] $Window,

        [Parameter(Mandatory = $true)]
        [string] $AutomationId
    )

    $condition = [Windows.Automation.PropertyCondition]::new(
        [Windows.Automation.AutomationElement]::AutomationIdProperty,
        $AutomationId
    )
    return $Window.FindFirst(
        [Windows.Automation.TreeScope]::Descendants,
        $condition
    )
}

function New-ProbeImage {
    param(
        [Parameter(Mandatory = $true)]
        [string] $Path
    )

    $bitmap = [Drawing.Bitmap]::new(320, 120)
    $graphics = [Drawing.Graphics]::FromImage($bitmap)
    try {
        $graphics.Clear([Drawing.Color]::Orange)
        $graphics.FillRectangle([Drawing.Brushes]::Blue, 0, 0, 70, 120)
        $bitmap.Save($Path, [Drawing.Imaging.ImageFormat]::Png)
    } finally {
        $graphics.Dispose()
        $bitmap.Dispose()
    }
}

function Get-RenderedColorBounds {
    param(
        [Parameter(Mandatory = $true)]
        [Windows.Automation.AutomationElement] $Window
    )

    $rectangle = $Window.Current.BoundingRectangle
    if ($rectangle.IsEmpty) {
        throw "The PicView window had no screen bounds."
    }
    $capture = [Drawing.Bitmap]::new(
        [int]$rectangle.Width,
        [int]$rectangle.Height
    )
    $graphics = [Drawing.Graphics]::FromImage($capture)
    try {
        $graphics.CopyFromScreen(
            [int]$rectangle.X,
            [int]$rectangle.Y,
            0,
            0,
            $capture.Size
        )
    } finally {
        $graphics.Dispose()
    }

    $minimumX = $capture.Width
    $minimumY = $capture.Height
    $maximumX = -1
    $maximumY = -1
    try {
        for ($y = 0; $y -lt $capture.Height; $y++) {
            for ($x = 0; $x -lt $capture.Width; $x++) {
                $pixel = $capture.GetPixel($x, $y)
                $isBlue = (
                    $pixel.B -gt 200 -and
                    $pixel.R -lt 50 -and
                    $pixel.G -lt 50
                )
                $isOrange = (
                    $pixel.R -gt 200 -and
                    $pixel.G -gt 100 -and
                    $pixel.G -lt 200 -and
                    $pixel.B -lt 50
                )
                if (-not ($isBlue -or $isOrange)) {
                    continue
                }
                $minimumX = [Math]::Min($minimumX, $x)
                $minimumY = [Math]::Min($minimumY, $y)
                $maximumX = [Math]::Max($maximumX, $x)
                $maximumY = [Math]::Max($maximumY, $y)
            }
        }
    } finally {
        $capture.Dispose()
    }
    if ($maximumX -lt $minimumX -or $maximumY -lt $minimumY) {
        throw "The seeded image colors were absent from the rendered window."
    }
    return [ordered]@{
        width = $maximumX - $minimumX + 1
        height = $maximumY - $minimumY + 1
    }
}

function Set-ForegroundWindowExact {
    param(
        [Parameter(Mandatory = $true)]
        [IntPtr] $Handle,

        [Parameter(Mandatory = $true)]
        [string] $Label
    )

    if (-not [ReproitPicViewInput]::SetForegroundWindow($Handle)) {
        throw "SetForegroundWindow rejected the $Label window."
    }
    Start-Sleep -Milliseconds 500
    $actual = [ReproitPicViewInput]::GetForegroundWindow()
    if ($actual -ne $Handle) {
        throw "The $Label window did not own foreground input."
    }
    return $actual
}

try {
    foreach ($requiredPath in @($applicationPath, $applicationAssemblyPath)) {
        if (-not (Test-Path -LiteralPath $requiredPath -PathType Leaf)) {
            throw "Required PicView file does not exist: $requiredPath"
        }
    }
    $result.exactIdentity.executableSha256 =
        (Get-FileHash -LiteralPath $applicationPath -Algorithm SHA256).Hash
    $result.exactIdentity.applicationAssemblySha256 =
        (Get-FileHash -LiteralPath $applicationAssemblyPath -Algorithm SHA256).Hash
    if (
        $result.exactIdentity.applicationAssemblySha256 -ne
        $ExpectedApplicationSha256.ToUpperInvariant()
    ) {
        throw "PicView.dll does not match the expected exact revision."
    }
    $result.assertions += "application-assembly-matches-exact-revision"

    $initialProcesses = @(Get-Process -Name "PicView" -ErrorAction SilentlyContinue)
    $result.containment.initialPicViewProcessCount = $initialProcesses.Count
    if ($initialProcesses.Count -ne 0) {
        throw "Refusing to start while PicView is already running."
    }
    $result.assertions += "no-preexisting-picview-process"

    $result.containment.configAbsentBeforeRun =
        -not (Test-Path -LiteralPath $configPath)
    if (-not $result.containment.configAbsentBeforeRun) {
        throw "PicView roaming state must be absent before every run."
    }
    $result.containment.runRootAbsentBeforeRun =
        -not (Test-Path -LiteralPath $runRoot)
    if (-not $result.containment.runRootAbsentBeforeRun) {
        throw "The owned run root was not fresh."
    }
    New-Item -ItemType Directory -Path $runRoot | Out-Null
    New-ProbeImage -Path $imagePath
    $result.observation.initialFileSha256 =
        (Get-FileHash -LiteralPath $imagePath -Algorithm SHA256).Hash
    $result.assertions += "fresh-config-and-seeded-local-image"

    $env:DOTNET_ROOT = (Resolve-Path -LiteralPath $DotnetRoot).Path
    $env:PATH = "$env:DOTNET_ROOT;$env:PATH"
    $applicationProcess = Start-Process `
        -FilePath $applicationPath `
        -ArgumentList "`"$imagePath`"" `
        -WorkingDirectory $applicationDirectory `
        -PassThru

    $window = Wait-ForMainWindow -Process $applicationProcess
    $applicationProcess.Refresh()
    $result.readiness.processId = $applicationProcess.Id
    $result.readiness.processPath = $applicationProcess.Path
    $result.readiness.processSessionId = $applicationProcess.SessionId
    $result.readiness.windowNameBeforeAction = $window.Current.Name
    $result.readiness.elapsedMilliseconds = $stopwatch.ElapsedMilliseconds
    if ($applicationProcess.SessionId -ne $result.environment.sessionId) {
        throw "PicView did not start in the harness desktop session."
    }
    if (
        -not $applicationProcess.Path.StartsWith(
            $applicationDirectory,
            [StringComparison]::OrdinalIgnoreCase
        )
    ) {
        throw "PicView escaped the exact application directory."
    }

    $mainView = Get-ElementByAutomationId -Window $window -AutomationId "MainView"
    $title = Get-ElementByAutomationId -Window $window -AutomationId "TextBlock"
    if ($null -eq $mainView -or $null -eq $title) {
        throw "PicView did not publish the required Avalonia peers to UIA."
    }
    $result.readiness.mainViewAutomationId = $mainView.Current.AutomationId
    $result.readiness.titleAutomationId = $title.Current.AutomationId
    $result.assertions += "uia-main-view-and-title-ready"

    $focusProcess = Start-Process `
        -FilePath "cmd.exe" `
        -ArgumentList "/k title ReproitPicViewFocus-$PID-$Run" `
        -PassThru
    $focusHandle = Wait-ForFocusHelper -Process $focusProcess
    $applicationProcess.Refresh()
    $applicationHandle = $applicationProcess.MainWindowHandle
    $result.action.appHandle = $applicationHandle.ToInt64()
    $result.action.focusHelperHandle = $focusHandle.ToInt64()

    $focused = Set-ForegroundWindowExact `
        -Handle $applicationHandle `
        -Label "PicView"
    $result.action.foregroundAfterAppFocus = $focused.ToInt64()
    if ($Role -eq "control") {
        [ReproitPicViewInput]::keybd_event(
            $sVirtualKey,
            0,
            0,
            [UIntPtr]::Zero
        )
        [ReproitPicViewInput]::keybd_event(
            $sVirtualKey,
            0,
            $keyUpFlag,
            [UIntPtr]::Zero
        )
    } else {
        [ReproitPicViewInput]::keybd_event(
            $controlVirtualKey,
            0,
            0,
            [UIntPtr]::Zero
        )
        Start-Sleep -Milliseconds 500
        $focused = Set-ForegroundWindowExact `
            -Handle $focusHandle `
            -Label "focus helper"
        $result.action.foregroundAfterHelperFocus = $focused.ToInt64()
        $focused = Set-ForegroundWindowExact `
            -Handle $applicationHandle `
            -Label "PicView return"
        $result.action.foregroundAfterReturnFocus = $focused.ToInt64()
        [ReproitPicViewInput]::keybd_event(
            $sVirtualKey,
            0,
            0,
            [UIntPtr]::Zero
        )
        [ReproitPicViewInput]::keybd_event(
            $sVirtualKey,
            0,
            $keyUpFlag,
            [UIntPtr]::Zero
        )
        [ReproitPicViewInput]::keybd_event(
            $controlVirtualKey,
            0,
            $keyUpFlag,
            [UIntPtr]::Zero
        )
    }
    Start-Sleep -Seconds 2

    if ($applicationProcess.HasExited) {
        throw "PicView exited before the reached observation."
    }
    $title = Get-ElementByAutomationId -Window $window -AutomationId "TextBlock"
    if ($null -eq $title) {
        throw "The title UIA peer disappeared after the keyboard action."
    }
    $result.observation.finalFileSha256 =
        (Get-FileHash -LiteralPath $imagePath -Algorithm SHA256).Hash
    $result.observation.fileChanged = (
        $result.observation.initialFileSha256 -ne
        $result.observation.finalFileSha256
    )
    $result.observation.windowNameAfterAction = $window.Current.Name
    $result.observation.titleNameAfterAction = $title.Current.Name
    $bounds = Get-RenderedColorBounds -Window $window
    $result.observation.renderedColorWidth = $bounds.width
    $result.observation.renderedColorHeight = $bounds.height
    $result.observation.observationReached = $true

    if (
        $result.observation.fileChanged -ne
        $result.observation.expectedFileChanged
    ) {
        throw "The observed save behavior did not match the selected revision role."
    }
    if ($Role -eq "fixed") {
        if ($bounds.width -le $bounds.height) {
            throw "The fixed build rotated the landscape image during Ctrl+S."
        }
    } else {
        if ($bounds.height -le $bounds.width) {
            throw "The affected or control build did not perform the bare-S rotation."
        }
    }
    $result.assertions += "save-or-rotation-behavior-matches-role"
    $result.assertions += "uia-observation-remained-reachable"
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
    [ReproitPicViewInput]::keybd_event(
        $controlVirtualKey,
        0,
        $keyUpFlag,
        [UIntPtr]::Zero
    )
    $result.containment.controlKeyReleased = $true
    if ($null -ne $applicationProcess) {
        Stop-Process `
            -Id $applicationProcess.Id `
            -Force `
            -ErrorAction SilentlyContinue
        $result.containment.stoppedApplicationProcess = $true
    }
    if ($null -ne $focusProcess) {
        Stop-Process `
            -Id $focusProcess.Id `
            -Force `
            -ErrorAction SilentlyContinue
        $result.containment.stoppedFocusProcess = $true
    }
    foreach ($ownedProcess in @(Get-OwnedApplicationProcesses)) {
        Stop-Process `
            -Id ([int]$ownedProcess.ProcessId) `
            -Force `
            -ErrorAction SilentlyContinue
    }
    Remove-Item -LiteralPath $configPath -Recurse -Force -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath $runRoot -Recurse -Force -ErrorAction SilentlyContinue
    $result.containment.remainingApplicationProcessCount =
        @(Get-OwnedApplicationProcesses).Count
    $result.containment.configRemoved =
        -not (Test-Path -LiteralPath $configPath)
    $result.containment.runRootRemoved =
        -not (Test-Path -LiteralPath $runRoot)
    $stopwatch.Stop()
    $result.finishedAt = [DateTimeOffset]::UtcNow.ToString("O")
    $result.elapsedSeconds = $stopwatch.Elapsed.TotalSeconds
    $result.exceptions =
        if ($null -eq $failure) { @() } else { @($failure.Exception.Message) }
    $outputDirectory = Split-Path -Parent $OutputPath
    if (-not [string]::IsNullOrWhiteSpace($outputDirectory)) {
        New-Item -ItemType Directory -Force -Path $outputDirectory | Out-Null
    }
    $result |
        ConvertTo-Json -Depth 12 |
        Set-Content -LiteralPath $OutputPath -Encoding utf8
}

if ($null -ne $failure) {
    throw $failure
}
