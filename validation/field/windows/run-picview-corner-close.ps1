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

public static class ReproitPicViewCornerInput
{
    [StructLayout(LayoutKind.Sequential)]
    public struct Point
    {
        public int X;
        public int Y;
    }

    [StructLayout(LayoutKind.Sequential)]
    public struct MouseInput
    {
        public int X;
        public int Y;
        public uint Data;
        public uint Flags;
        public uint Time;
        public UIntPtr ExtraInfo;
    }

    [StructLayout(LayoutKind.Explicit)]
    public struct InputUnion
    {
        [FieldOffset(0)]
        public MouseInput Mouse;
    }

    [StructLayout(LayoutKind.Sequential)]
    public struct Input
    {
        public uint Type;
        public InputUnion Union;
    }

    [DllImport("user32.dll")]
    public static extern IntPtr GetForegroundWindow();

    [DllImport("user32.dll")]
    public static extern uint GetWindowThreadProcessId(
        IntPtr window,
        out uint processId
    );

    [DllImport("user32.dll")]
    public static extern bool GetCursorPos(out Point point);

    [DllImport("user32.dll")]
    public static extern bool SetCursorPos(int x, int y);

    [DllImport("user32.dll")]
    public static extern bool SetForegroundWindow(IntPtr window);

    [DllImport("user32.dll")]
    public static extern bool BringWindowToTop(IntPtr window);

    [DllImport("user32.dll")]
    public static extern bool ShowWindowAsync(IntPtr window, int command);

    [DllImport("user32.dll", SetLastError = true)]
    private static extern uint SendInput(
        uint inputCount,
        Input[] inputs,
        int inputSize
    );

    public static bool SendMouseInput(uint flags)
    {
        Input input = new Input();
        input.Type = 0;
        input.Union.Mouse.Flags = flags;
        return SendInput(1, new Input[] { input }, Marshal.SizeOf<Input>()) == 1;
    }
}
"@

$applicationDirectory = (Resolve-Path -LiteralPath $ApplicationDirectory).Path
$applicationPath = Join-Path $applicationDirectory "PicView.exe"
$applicationAssemblyPath = Join-Path $applicationDirectory "PicView.dll"
$configPath = Join-Path $env:APPDATA "Ruben2776\PicView"
$runRoot = Join-Path $env:TEMP "reproit-picview-corner-$PID-$Role-$Run"
$imagePath = Join-Path $runRoot "corner-probe.png"
$firewallPrefix = "ReproitAvalonia-PicViewCorner-$PID-$Role-$Run"
$firewallRules = [Collections.Generic.List[string]]::new()
$applicationProcess = $null
$failure = $null
$startedAt = [DateTimeOffset]::UtcNow
$stopwatch = [Diagnostics.Stopwatch]::StartNew()
$originalPointer = [ReproitPicViewCornerInput+Point]::new()
[void][ReproitPicViewCornerInput]::GetCursorPos([ref]$originalPointer)

$result = [ordered]@{
    schemaVersion = 1
    campaign = "picview-corner-close-targets-320"
    target = "windows-avalonia"
    role = $Role
    run = $Run
    status = "running"
    startedAt = $startedAt.ToString("O")
    finishedAt = $null
    elapsedSeconds = $null
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
        automation =
            "Windows UI Automation with one foreground Win32 SendInput click"
        networkPolicy =
            "program-scoped Windows Firewall inbound and outbound block rules"
    }
    containment = [ordered]@{
        initialProcessCount = $null
        configAbsentBeforeRun = $null
        runRootAbsentBeforeRun = $null
        inboundFirewallRuleActive = $false
        outboundFirewallRuleActive = $false
        stoppedApplicationProcess = $false
        remainingApplicationProcessCount = $null
        configRemoved = $null
        runRootRemoved = $null
        pointerRestored = $false
        firewallRulesRemoved = $null
    }
    readiness = [ordered]@{
        processId = $null
        processPath = $null
        processSessionId = $null
        mainWindowName = $null
        mainViewAutomationId = $null
        closeButtonAutomationId = $null
        elapsedMilliseconds = $null
    }
    preconditions = [ordered]@{
        subject = "WinMainWindow"
        handlerAttachment =
            "CaptionButtonCornerHandler.Attach(this, visibility predicate)"
        closeTargetWidthPixels = 30
        closeTargetHeightPixels = 31
        source =
            "src/PicView.Avalonia.Win32/Views/WinMainWindow.axaml.cs"
    }
    action = [ordered]@{
        triggerClick = [ordered]@{
            kind = if ($Role -eq "control") {
                "close-button center"
            } else {
                "window top-right corner"
            }
            x = $null
            y = $null
            foregroundProcessId = $null
            sendInputClickCount = 1
        }
        neighboringControl = $Role -eq "control"
    }
    observation = [ordered]@{
        subject = "WinMainWindow"
        windowBounds = $null
        closeButtonBounds = $null
        cornerPointInsideWindow = $null
        windowPresentAfterClick = $null
        closed = $null
        expectedClosed = $Role -ne "affected"
        observationReached = $false
    }
    assertions = @()
    exceptions = @()
    failure = $null
}

function Get-OwnedProcesses {
    @(Get-CimInstance Win32_Process |
        Where-Object {
            $null -ne $_.ExecutablePath -and
            $_.ExecutablePath.StartsWith(
                $applicationDirectory,
                [StringComparison]::OrdinalIgnoreCase
            )
        })
}

function Get-ProcessWindows {
    param([int] $ProcessId)

    $condition = [Windows.Automation.PropertyCondition]::new(
        [Windows.Automation.AutomationElement]::ProcessIdProperty,
        $ProcessId
    )
    @(
        [Windows.Automation.AutomationElement]::RootElement.FindAll(
            [Windows.Automation.TreeScope]::Children,
            $condition
        )
    )
}

function Get-ElementByAutomationId {
    param(
        [Windows.Automation.AutomationElement] $Window,
        [string] $AutomationId
    )

    $condition = [Windows.Automation.PropertyCondition]::new(
        [Windows.Automation.AutomationElement]::AutomationIdProperty,
        $AutomationId
    )
    $Window.FindFirst(
        [Windows.Automation.TreeScope]::Descendants,
        $condition
    )
}

function Wait-ForMainWindow {
    param([Diagnostics.Process] $Process)

    $deadline = [DateTimeOffset]::UtcNow.AddSeconds(30)
    while ([DateTimeOffset]::UtcNow -lt $deadline) {
        $Process.Refresh()
        foreach ($window in @(Get-ProcessWindows -ProcessId $Process.Id)) {
            $mainView = Get-ElementByAutomationId `
                -Window $window `
                -AutomationId "MainView"
            if ($null -ne $mainView) {
                return [ordered]@{
                    window = $window
                    mainView = $mainView
                }
            }
        }
        Start-Sleep -Milliseconds 250
    }
    throw "PicView did not publish its main window and MainView to UIA."
}

function Test-WindowPresent {
    param(
        [int] $ProcessId,
        [int] $WindowHandle
    )

    foreach ($window in @(Get-ProcessWindows -ProcessId $ProcessId)) {
        if ($window.Current.NativeWindowHandle -eq $WindowHandle) {
            return $true
        }
    }
    return $false
}

function Get-RectangleRecord {
    param([Windows.Automation.AutomationElement] $Element)

    $rectangle = $Element.Current.BoundingRectangle
    [ordered]@{
        x = [int]$rectangle.X
        y = [int]$rectangle.Y
        width = [int]$rectangle.Width
        height = [int]$rectangle.Height
        right = [int]($rectangle.X + $rectangle.Width)
        bottom = [int]($rectangle.Y + $rectangle.Height)
    }
}

function New-ProbeImage {
    param([string] $Path)

    $bitmap = [Drawing.Bitmap]::new(64, 64)
    $graphics = [Drawing.Graphics]::FromImage($bitmap)
    try {
        $graphics.Clear([Drawing.Color]::FromArgb(12, 223, 91))
        $bitmap.Save($Path, [Drawing.Imaging.ImageFormat]::Png)
    } finally {
        $graphics.Dispose()
        $bitmap.Dispose()
    }
}

function Set-ForegroundWindowChecked {
    param(
        [IntPtr] $WindowHandle,
        [int] $ExpectedProcessId
    )

    [void][ReproitPicViewCornerInput]::ShowWindowAsync($WindowHandle, 9)
    [void][ReproitPicViewCornerInput]::BringWindowToTop($WindowHandle)
    [void][ReproitPicViewCornerInput]::SetForegroundWindow($WindowHandle)
    Start-Sleep -Milliseconds 500
    $foreground = [ReproitPicViewCornerInput]::GetForegroundWindow()
    [uint32]$foregroundProcessId = 0
    [void][ReproitPicViewCornerInput]::GetWindowThreadProcessId(
        $foreground,
        [ref]$foregroundProcessId
    )
    if ($foregroundProcessId -ne $ExpectedProcessId) {
        throw "Win32 foreground activation did not focus the PicView window."
    }
    return $foregroundProcessId
}

function Send-LeftClick {
    param(
        [int] $X,
        [int] $Y
    )

    [void][ReproitPicViewCornerInput]::SetCursorPos($X, $Y)
    Start-Sleep -Milliseconds 250
    if (-not [ReproitPicViewCornerInput]::SendMouseInput(2)) {
        throw "SendInput rejected the foreground button-down."
    }
    Start-Sleep -Milliseconds 30
    if (-not [ReproitPicViewCornerInput]::SendMouseInput(4)) {
        throw "SendInput rejected the foreground button-up."
    }
}

try {
    foreach ($path in @($applicationPath, $applicationAssemblyPath)) {
        if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
            throw "Required PicView path is missing: $path"
        }
    }
    $result.exactIdentity.executableSha256 =
        (Get-FileHash $applicationPath -Algorithm SHA256).Hash
    $result.exactIdentity.applicationAssemblySha256 =
        (Get-FileHash $applicationAssemblyPath -Algorithm SHA256).Hash
    if (
        $result.exactIdentity.applicationAssemblySha256 -ne
        $ExpectedApplicationSha256.ToUpperInvariant()
    ) {
        throw "PicView.dll does not match the expected exact revision."
    }
    $result.assertions += "application-assembly-matches-exact-revision"

    $initialProcesses = @(Get-Process -Name "PicView" -ErrorAction SilentlyContinue)
    $result.containment.initialProcessCount = $initialProcesses.Count
    if ($initialProcesses.Count -ne 0) {
        throw "Refusing to start while PicView is already running."
    }
    $result.containment.configAbsentBeforeRun = -not (Test-Path $configPath)
    $result.containment.runRootAbsentBeforeRun = -not (Test-Path $runRoot)
    if (
        -not $result.containment.configAbsentBeforeRun -or
        -not $result.containment.runRootAbsentBeforeRun
    ) {
        throw "PicView state was not fresh before the run."
    }
    New-Item -ItemType Directory -Path $runRoot | Out-Null
    New-ProbeImage -Path $imagePath

    foreach ($direction in @("Inbound", "Outbound")) {
        $ruleName = "$firewallPrefix-$direction"
        New-NetFirewallRule `
            -DisplayName $ruleName `
            -Direction $direction `
            -Action Block `
            -Enabled True `
            -Profile Any `
            -Program $applicationPath | Out-Null
        $firewallRules.Add($ruleName)
    }
    $result.containment.inboundFirewallRuleActive =
        (Get-NetFirewallRule -DisplayName "$firewallPrefix-Inbound").Enabled -eq
            "True"
    $result.containment.outboundFirewallRuleActive =
        (Get-NetFirewallRule -DisplayName "$firewallPrefix-Outbound").Enabled -eq
            "True"
    if (
        -not $result.containment.inboundFirewallRuleActive -or
        -not $result.containment.outboundFirewallRuleActive
    ) {
        throw "PicView firewall containment was not active."
    }

    $env:DOTNET_ROOT = (Resolve-Path -LiteralPath $DotnetRoot).Path
    $env:PATH = "$env:DOTNET_ROOT;$env:PATH"
    $applicationProcess = Start-Process `
        -FilePath $applicationPath `
        -ArgumentList "`"$imagePath`"" `
        -WorkingDirectory $applicationDirectory `
        -PassThru
    $main = Wait-ForMainWindow -Process $applicationProcess
    $applicationProcess.Refresh()
    $mainWindow = $main.window
    $mainView = $main.mainView
    $mainWindowHandle = $mainWindow.Current.NativeWindowHandle
    $result.readiness.processId = $applicationProcess.Id
    $result.readiness.processPath = $applicationProcess.Path
    $result.readiness.processSessionId = $applicationProcess.SessionId
    $result.readiness.mainWindowName = $mainWindow.Current.Name
    $result.readiness.mainViewAutomationId = $mainView.Current.AutomationId
    if ($applicationProcess.SessionId -ne $result.environment.sessionId) {
        throw "PicView did not start in the interactive harness session."
    }

    $result.action.triggerClick.foregroundProcessId =
        Set-ForegroundWindowChecked `
        -WindowHandle ([IntPtr]::new($mainWindowHandle)) `
        -ExpectedProcessId $applicationProcess.Id
    $closeButton = Get-ElementByAutomationId `
        -Window $mainWindow `
        -AutomationId "CloseButton"
    if ($null -eq $closeButton) {
        throw "PicView did not publish its main CloseButton to UIA."
    }
    $windowBounds = Get-RectangleRecord -Element $mainWindow
    $closeButtonBounds = Get-RectangleRecord -Element $closeButton
    $result.readiness.closeButtonAutomationId =
        $closeButton.Current.AutomationId
    $result.readiness.elapsedMilliseconds = $stopwatch.ElapsedMilliseconds
    $result.observation.windowBounds = $windowBounds
    $result.observation.closeButtonBounds = $closeButtonBounds
    $result.assertions += "uia-main-window-and-close-button-ready"

    if ($Role -eq "control") {
        $clickX = [int](
            $closeButtonBounds.x + ($closeButtonBounds.width / 2)
        )
        $clickY = [int](
            $closeButtonBounds.y + ($closeButtonBounds.height / 2)
        )
    } else {
        $clickX = $windowBounds.right - 1
        $clickY = $windowBounds.y + 1
    }
    $result.observation.cornerPointInsideWindow = (
        $clickX -ge $windowBounds.x -and
        $clickX -lt $windowBounds.right -and
        $clickY -ge $windowBounds.y -and
        $clickY -lt $windowBounds.bottom
    )
    if (-not $result.observation.cornerPointInsideWindow) {
        throw "The selected foreground click point was outside WinMainWindow."
    }

    $result.action.triggerClick.foregroundProcessId =
        Set-ForegroundWindowChecked `
            -WindowHandle ([IntPtr]::new($mainWindowHandle)) `
            -ExpectedProcessId $applicationProcess.Id
    $result.action.triggerClick.x = $clickX
    $result.action.triggerClick.y = $clickY
    Send-LeftClick -X $clickX -Y $clickY

    $deadline = [DateTimeOffset]::UtcNow.AddSeconds(3)
    $windowPresent = $true
    while ([DateTimeOffset]::UtcNow -lt $deadline) {
        $windowPresent = Test-WindowPresent `
            -ProcessId $applicationProcess.Id `
            -WindowHandle $mainWindowHandle
        if (-not $windowPresent) {
            break
        }
        Start-Sleep -Milliseconds 100
    }
    $result.observation.windowPresentAfterClick = $windowPresent
    $result.observation.closed = -not $windowPresent
    $result.observation.observationReached = $true
    if ($result.observation.closed -ne $result.observation.expectedClosed) {
        throw "The observed corner-close behavior did not match the role."
    }
    $result.assertions += "foreground-sendinput-trigger-click-reached"
    $result.assertions += "uia-window-presence-matches-role"
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
    $result.containment.pointerRestored =
        [ReproitPicViewCornerInput]::SetCursorPos(
            $originalPointer.X,
            $originalPointer.Y
        )
    if ($null -ne $applicationProcess) {
        Stop-Process -Id $applicationProcess.Id -Force -ErrorAction SilentlyContinue
        $result.containment.stoppedApplicationProcess = $true
    }
    foreach ($ownedProcess in @(Get-OwnedProcesses)) {
        Stop-Process `
            -Id ([int]$ownedProcess.ProcessId) `
            -Force `
            -ErrorAction SilentlyContinue
    }
    foreach ($ruleName in $firewallRules) {
        Remove-NetFirewallRule `
            -DisplayName $ruleName `
            -ErrorAction SilentlyContinue
    }
    Remove-Item $configPath -Recurse -Force -ErrorAction SilentlyContinue
    Remove-Item $runRoot -Recurse -Force -ErrorAction SilentlyContinue
    $result.containment.remainingApplicationProcessCount =
        @(Get-OwnedProcesses).Count
    $result.containment.configRemoved = -not (Test-Path $configPath)
    $result.containment.runRootRemoved = -not (Test-Path $runRoot)
    $result.containment.firewallRulesRemoved =
        @(
            Get-NetFirewallRule -ErrorAction SilentlyContinue |
                Where-Object { $_.DisplayName -like "$firewallPrefix-*" }
        ).Count -eq 0
    if (
        $result.containment.remainingApplicationProcessCount -ne 0 -or
        -not $result.containment.configRemoved -or
        -not $result.containment.runRootRemoved -or
        -not $result.containment.pointerRestored -or
        -not $result.containment.firewallRulesRemoved
    ) {
        $result.status = "fail"
        if ($null -eq $result.failure) {
            $result.failure = [ordered]@{
                message = "PicView corner-close containment cleanup failed."
                category = "ResourceUnavailable"
                stack = $null
            }
        }
    }
    $stopwatch.Stop()
    $result.finishedAt = [DateTimeOffset]::UtcNow.ToString("O")
    $result.elapsedSeconds = $stopwatch.Elapsed.TotalSeconds
    if ($null -eq $failure) {
        $result.exceptions = [object[]]@()
    } else {
        $result.exceptions = [object[]]@($failure.Exception.Message)
    }
    $outputDirectory = Split-Path -Parent $OutputPath
    New-Item -ItemType Directory -Force -Path $outputDirectory | Out-Null
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
