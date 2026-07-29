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
    [ValidateScript({ Test-Path -LiteralPath $_ -PathType Leaf })]
    [string] $SubjectAssembly,

    [Parameter(Mandatory = $true)]
    [ValidatePattern("^[0-9A-Fa-f]{64}$")]
    [string] $ExpectedSubjectSha256,

    [Parameter(Mandatory = $true)]
    [ValidateSet("affected", "fixed", "control")]
    [string] $Role,

    [Parameter(Mandatory = $true)]
    [ValidateRange(1, 3)]
    [int] $Run,

    [Parameter(Mandatory = $true)]
    [string] $OutputPath,

    [ValidateScript({ Test-Path -LiteralPath $_ -PathType Container })]
    [string] $DotnetRoot = "C:\lab\dotnet10"
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"

Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes
Add-Type -AssemblyName System.Drawing
Add-Type -AssemblyName System.Windows.Forms
Add-Type @"
using System;
using System.Runtime.InteropServices;

public static class ReproitILSpyInput
{
    [StructLayout(LayoutKind.Sequential)]
    public struct Point
    {
        public int X;
        public int Y;
    }

    [DllImport("user32.dll")]
    public static extern IntPtr GetForegroundWindow();

    [DllImport("user32.dll")]
    public static extern bool GetCursorPos(out Point point);

    [DllImport("user32.dll")]
    public static extern bool SetForegroundWindow(IntPtr window);

    [DllImport("user32.dll")]
    public static extern bool SetCursorPos(int x, int y);

    [DllImport("user32.dll")]
    public static extern void mouse_event(
        uint flags,
        uint x,
        uint y,
        uint data,
        UIntPtr extraInfo
    );
}
"@

$applicationDirectory = (Resolve-Path -LiteralPath $ApplicationDirectory).Path
$applicationPath = Join-Path $applicationDirectory "ILSpy.exe"
$applicationAssemblyPath = Join-Path $applicationDirectory "ILSpy.dll"
$subjectAssembly = (Resolve-Path -LiteralPath $SubjectAssembly).Path
$subjectXml = [IO.Path]::ChangeExtension($subjectAssembly, ".xml")
$runRoot = Join-Path $env:TEMP "reproit-ilspy-$PID-$Role-$Run"
$configPath = Join-Path $runRoot "config.xml"
$beforeScreenshot = Join-Path $runRoot "before.png"
$afterScreenshot = Join-Path $runRoot "after.png"
$firewallPrefix = "ReproitAvalonia-ILSpy-$PID-$Role-$Run"
$firewallRules = [System.Collections.Generic.List[string]]::new()
$startedAt = [DateTimeOffset]::UtcNow
$stopwatch = [Diagnostics.Stopwatch]::StartNew()
$applicationProcess = $null
$failure = $null
$originalPointer = [ReproitILSpyInput+Point]::new()
[void][ReproitILSpyInput]::GetCursorPos([ref]$originalPointer)

$result = [ordered]@{
    schemaVersion = 1
    campaign = "ilspy-fold-documentation-749"
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
        subjectAssemblyPath = $subjectAssembly
        subjectAssemblySha256 = $null
        subjectXmlSha256 = $null
    }
    environment = [ordered]@{
        os = [Environment]::OSVersion.VersionString
        architecture =
            [Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString()
        sessionId = (Get-Process -Id $PID).SessionId
        dotnetSdk = (& (Join-Path $DotnetRoot "dotnet.exe") --version)
        automation = "Windows UI Automation"
        networkPolicy =
            "program-scoped Windows Firewall inbound and outbound block rules"
    }
    containment = [ordered]@{
        initialProcessCount = $null
        runRootAbsentBeforeRun = $null
        stoppedApplicationProcess = $false
        remainingApplicationProcessCount = $null
        runRootRemoved = $null
        pointerRestored = $null
        inboundFirewallRuleActive = $false
        outboundFirewallRuleActive = $false
        firewallRulesRemoved = $null
    }
    readiness = [ordered]@{
        processId = $null
        processPath = $null
        processSessionId = $null
        windowName = $null
        editorAutomationId = $null
        editorWidth = $null
        editorHeight = $null
        elapsedMilliseconds = $null
    }
    action = [ordered]@{
        kind =
            if ($Role -eq "control") {
                "expand the independent XML documentation fold"
            } else {
                "expand the member and XML documentation, then toggle the body fold"
            }
        menuItemName = $null
        bodyTogglePoint = $null
        neighboringControl = $Role -eq "control"
    }
    observation = [ordered]@{
        minimizedTrigger =
            if ($Role -eq "control") {
                "click the XML documentation folding margin once"
            } else {
                "expand XML documentation and the method body, then toggle inside the body"
            }
        expectedDocumentationState =
            if ($Role -eq "fixed") { "collapsed" } else { "expanded" }
        before = $null
        after = $null
        observationReached = $false
    }
    assertions = @()
    exceptions = @()
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

function Wait-ForApplicationWindow {
    param([Diagnostics.Process] $Process)

    $condition = [Windows.Automation.PropertyCondition]::new(
        [Windows.Automation.AutomationElement]::ProcessIdProperty,
        $Process.Id
    )
    $deadline = [DateTimeOffset]::UtcNow.AddSeconds(40)
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
    throw "ILSpy did not publish a main window to UI Automation."
}

function Wait-ForDecompilerEditor {
    param([Windows.Automation.AutomationElement] $Window)

    $condition = [Windows.Automation.PropertyCondition]::new(
        [Windows.Automation.AutomationElement]::AutomationIdProperty,
        "self"
    )
    $deadline = [DateTimeOffset]::UtcNow.AddSeconds(30)
    while ([DateTimeOffset]::UtcNow -lt $deadline) {
        $editor = $Window.FindFirst(
            [Windows.Automation.TreeScope]::Descendants,
            $condition
        )
        if ($null -ne $editor) {
            return $editor
        }
        Start-Sleep -Milliseconds 250
    }
    throw "ILSpy did not publish its AvaloniaEdit peer to UI Automation."
}

function Click-EditorPoint {
    param(
        [Windows.Automation.AutomationElement] $Editor,
        [int] $OffsetX,
        [int] $OffsetY
    )

    $rectangle = $Editor.Current.BoundingRectangle
    if ($rectangle.IsEmpty) {
        throw "The ILSpy editor had no screen bounds."
    }
    if (-not [ReproitILSpyInput]::SetCursorPos(
        [int]($rectangle.X + $OffsetX),
        [int]($rectangle.Y + $OffsetY)
    )) {
        throw "Could not position the pointer over the ILSpy editor."
    }
    [ReproitILSpyInput]::mouse_event(2, 0, 0, 0, [UIntPtr]::Zero)
    [ReproitILSpyInput]::mouse_event(4, 0, 0, 0, [UIntPtr]::Zero)
}

function Get-VisualSnapshot {
    param(
        [Windows.Automation.AutomationElement] $Editor,
        [string] $Path
    )

    $rectangle = $Editor.Current.BoundingRectangle
    $width = [Math]::Max(1, [int]$rectangle.Width)
    $height = [Math]::Max(1, [int]$rectangle.Height)
    $bitmap = [Drawing.Bitmap]::new($width, $height)
    $graphics = [Drawing.Graphics]::FromImage($bitmap)
    try {
        $graphics.CopyFromScreen(
            [int]$rectangle.X,
            [int]$rectangle.Y,
            0,
            0,
            [Drawing.Size]::new($width, $height)
        )
        $bitmap.Save($Path, [Drawing.Imaging.ImageFormat]::Png)

        $colorCounts = @{}
        for ($y = 0; $y -lt $height; $y += 8) {
            for ($x = 30; $x -lt ($width - 20); $x += 8) {
                $pixel = $bitmap.GetPixel($x, $y)
                $key = "{0},{1},{2}" -f (
                    [Math]::Floor($pixel.R / 8) * 8
                ), (
                    [Math]::Floor($pixel.G / 8) * 8
                ), (
                    [Math]::Floor($pixel.B / 8) * 8
                )
                $colorCounts[$key] = 1 + [int]$colorCounts[$key]
            }
        }
        $backgroundKey = (
            $colorCounts.GetEnumerator() |
                Sort-Object Value -Descending |
                Select-Object -First 1
        ).Key
        $parts = $backgroundKey -split ","
        $activeRows = 0
        for ($y = 5; $y -lt ($height - 5); $y++) {
            $inkPixels = 0
            for ($x = 30; $x -lt ($width - 20); $x += 2) {
                $pixel = $bitmap.GetPixel($x, $y)
                $difference = [Math]::Max(
                    [Math]::Abs([int]$pixel.R - [int]$parts[0]),
                    [Math]::Max(
                        [Math]::Abs([int]$pixel.G - [int]$parts[1]),
                        [Math]::Abs([int]$pixel.B - [int]$parts[2])
                    )
                )
                if ($difference -gt 28) {
                    $inkPixels++
                }
            }
            if ($inkPixels -ge 8) {
                $activeRows++
            }
        }
        return [ordered]@{
            sha256 = (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash
            width = $width
            height = $height
            background = $backgroundKey
            activeRowCount = $activeRows
        }
    } finally {
        $graphics.Dispose()
        $bitmap.Dispose()
    }
}

function Invoke-ToggleFoldingMenu {
    param(
        [int] $ProcessId,
        [Windows.Automation.AutomationElement] $Editor,
        [int] $OffsetX,
        [int] $OffsetY
    )

    $rectangle = $Editor.Current.BoundingRectangle
    $screenX = [int]($rectangle.X + $OffsetX)
    $screenY = [int]($rectangle.Y + $OffsetY)
    [void][ReproitILSpyInput]::SetCursorPos($screenX, $screenY)
    [ReproitILSpyInput]::mouse_event(8, 0, 0, 0, [UIntPtr]::Zero)
    [ReproitILSpyInput]::mouse_event(16, 0, 0, 0, [UIntPtr]::Zero)

    $processCondition = [Windows.Automation.PropertyCondition]::new(
        [Windows.Automation.AutomationElement]::ProcessIdProperty,
        $ProcessId
    )
    $deadline = [DateTimeOffset]::UtcNow.AddSeconds(10)
    while ([DateTimeOffset]::UtcNow -lt $deadline) {
        $elements = [Windows.Automation.AutomationElement]::RootElement.FindAll(
            [Windows.Automation.TreeScope]::Descendants,
            $processCondition
        )
        for ($index = 0; $index -lt $elements.Count; $index++) {
            $element = $elements.Item($index)
            if ($element.Current.ControlType.ProgrammaticName -ne
                "ControlType.MenuItem") {
                continue
            }
            if ($element.Current.Name -ne "Toggle Folding") {
                continue
            }
            $bounds = $element.Current.BoundingRectangle
            if ($bounds.IsEmpty) {
                throw "The Toggle Folding menu item had no screen bounds."
            }
            [void][ReproitILSpyInput]::SetCursorPos(
                [int]($bounds.X + ($bounds.Width / 2)),
                [int]($bounds.Y + ($bounds.Height / 2))
            )
            [ReproitILSpyInput]::mouse_event(2, 0, 0, 0, [UIntPtr]::Zero)
            [ReproitILSpyInput]::mouse_event(4, 0, 0, 0, [UIntPtr]::Zero)
            return [ordered]@{
                name = $element.Current.Name
                x = $screenX
                y = $screenY
            }
        }
        Start-Sleep -Milliseconds 200
    }
    throw "The Toggle Folding menu item did not enter the UIA tree."
}

try {
    foreach ($requiredPath in @(
        $applicationPath,
        $applicationAssemblyPath,
        $subjectAssembly,
        $subjectXml
    )) {
        if (-not (Test-Path -LiteralPath $requiredPath -PathType Leaf)) {
            throw "Required ILSpy campaign file is missing: $requiredPath"
        }
    }
    $result.exactIdentity.executableSha256 =
        (Get-FileHash -LiteralPath $applicationPath -Algorithm SHA256).Hash
    $result.exactIdentity.applicationAssemblySha256 =
        (Get-FileHash -LiteralPath $applicationAssemblyPath -Algorithm SHA256).Hash
    $result.exactIdentity.subjectAssemblySha256 =
        (Get-FileHash -LiteralPath $subjectAssembly -Algorithm SHA256).Hash
    $result.exactIdentity.subjectXmlSha256 =
        (Get-FileHash -LiteralPath $subjectXml -Algorithm SHA256).Hash
    if (
        $result.exactIdentity.applicationAssemblySha256 -ne
        $ExpectedApplicationSha256.ToUpperInvariant()
    ) {
        throw "ILSpy.dll does not match the expected exact revision."
    }
    if (
        $result.exactIdentity.subjectAssemblySha256 -ne
        $ExpectedSubjectSha256.ToUpperInvariant()
    ) {
        throw "The folding subject does not match its expected hash."
    }
    $result.assertions += "application-and-subject-match-exact-hashes"

    $initialProcesses = @(Get-Process -Name "ILSpy" -ErrorAction SilentlyContinue)
    $result.containment.initialProcessCount = $initialProcesses.Count
    if ($initialProcesses.Count -ne 0) {
        throw "Refusing to start while ILSpy is already running."
    }
    $result.containment.runRootAbsentBeforeRun =
        -not (Test-Path -LiteralPath $runRoot)
    if (-not $result.containment.runRootAbsentBeforeRun) {
        throw "The ILSpy owned run root was not fresh."
    }
    New-Item -ItemType Directory -Path $runRoot | Out-Null

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
    $inboundRule = Get-NetFirewallRule `
        -DisplayName "$firewallPrefix-Inbound"
    $outboundRule = Get-NetFirewallRule `
        -DisplayName "$firewallPrefix-Outbound"
    $result.containment.inboundFirewallRuleActive =
        $inboundRule.Enabled -eq "True" -and $inboundRule.Action -eq "Block"
    $result.containment.outboundFirewallRuleActive =
        $outboundRule.Enabled -eq "True" -and $outboundRule.Action -eq "Block"
    if (
        -not $result.containment.inboundFirewallRuleActive -or
        -not $result.containment.outboundFirewallRuleActive
    ) {
        throw "ILSpy did not receive both active firewall block rules."
    }
    $result.assertions += "application-network-blocked-inbound-and-outbound"

    $env:DOTNET_ROOT = (Resolve-Path -LiteralPath $DotnetRoot).Path
    $env:PATH = "$env:DOTNET_ROOT;$env:PATH"
    $arguments = (
        "--newinstance -c `"$configPath`" " +
        "-n T:ReproitSubject.FoldSubject `"$subjectAssembly`""
    )
    $applicationProcess = Start-Process `
        -FilePath $applicationPath `
        -ArgumentList $arguments `
        -WorkingDirectory $applicationDirectory `
        -PassThru

    $window = Wait-ForApplicationWindow -Process $applicationProcess
    $editor = Wait-ForDecompilerEditor -Window $window
    $applicationProcess.Refresh()
    $result.readiness.processId = $applicationProcess.Id
    $result.readiness.processPath = $applicationProcess.Path
    $result.readiness.processSessionId = $applicationProcess.SessionId
    $result.readiness.windowName = $window.Current.Name
    $result.readiness.editorAutomationId = $editor.Current.AutomationId
    $editorRectangle = $editor.Current.BoundingRectangle
    $result.readiness.editorWidth = [int]$editorRectangle.Width
    $result.readiness.editorHeight = [int]$editorRectangle.Height
    $result.readiness.elapsedMilliseconds = $stopwatch.ElapsedMilliseconds
    if ($applicationProcess.SessionId -ne $result.environment.sessionId) {
        throw "ILSpy did not start in the harness desktop session."
    }
    if (
        -not $applicationProcess.Path.StartsWith(
            $applicationDirectory,
            [StringComparison]::OrdinalIgnoreCase
        )
    ) {
        throw "ILSpy escaped the exact application directory."
    }
    if (-not [ReproitILSpyInput]::SetForegroundWindow(
        $applicationProcess.MainWindowHandle
    )) {
        throw "ILSpy did not accept foreground focus."
    }
    Start-Sleep -Seconds 3
    $result.assertions += "uia-editor-ready-in-interactive-session"

    if ($Role -eq "control") {
        Click-EditorPoint -Editor $editor -OffsetX 24 -OffsetY 81
        Start-Sleep -Milliseconds 500
        [Windows.Forms.SendKeys]::SendWait("^{END}")
        Start-Sleep -Milliseconds 700
        $result.observation.after =
            Get-VisualSnapshot -Editor $editor -Path $afterScreenshot
        if ($result.observation.after.activeRowCount -le 150) {
            throw "The independent XML documentation control did not expand."
        }
    } else {
        Click-EditorPoint -Editor $editor -OffsetX 24 -OffsetY 117
        Start-Sleep -Milliseconds 500
        Click-EditorPoint -Editor $editor -OffsetX 24 -OffsetY 81
        Start-Sleep -Milliseconds 500
        [Windows.Forms.SendKeys]::SendWait("^{END}")
        Start-Sleep -Milliseconds 700
        $result.observation.before =
            Get-VisualSnapshot -Editor $editor -Path $beforeScreenshot
        if ($result.observation.before.activeRowCount -le 150) {
            throw "The campaign setup did not expand the XML documentation."
        }

        $menu = Invoke-ToggleFoldingMenu `
            -ProcessId $applicationProcess.Id `
            -Editor $editor `
            -OffsetX 100 `
            -OffsetY 450
        $result.action.menuItemName = $menu.name
        $result.action.bodyTogglePoint = [ordered]@{
            x = $menu.x
            y = $menu.y
        }
        $editorRectangle = $editor.Current.BoundingRectangle
        [void][ReproitILSpyInput]::SetCursorPos(
            [int]($editorRectangle.X + 640),
            [int]($editorRectangle.Y + 500)
        )
        Start-Sleep -Milliseconds 1600
        $result.observation.after =
            Get-VisualSnapshot -Editor $editor -Path $afterScreenshot

        if ($Role -eq "affected") {
            if ($result.observation.after.activeRowCount -le 150) {
                throw "The affected build unexpectedly collapsed the XML documentation."
            }
        } else {
            if ($result.observation.after.activeRowCount -ge 100) {
                throw "The fixed build left the XML documentation expanded."
            }
        }
    }

    $result.observation.observationReached = $true
    $result.assertions += "rendered-fold-state-matches-revision-role"
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
    if ($null -ne $applicationProcess) {
        Stop-Process `
            -Id $applicationProcess.Id `
            -Force `
            -ErrorAction SilentlyContinue
        $result.containment.stoppedApplicationProcess = $true
    }
    foreach ($ownedProcess in @(Get-OwnedApplicationProcesses)) {
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
    Remove-Item -LiteralPath $runRoot -Recurse -Force -ErrorAction SilentlyContinue
    [void][ReproitILSpyInput]::SetCursorPos($originalPointer.X, $originalPointer.Y)
    $restoredPointer = [ReproitILSpyInput+Point]::new()
    [void][ReproitILSpyInput]::GetCursorPos([ref]$restoredPointer)

    $result.containment.remainingApplicationProcessCount =
        @(Get-OwnedApplicationProcesses).Count
    $result.containment.runRootRemoved =
        -not (Test-Path -LiteralPath $runRoot)
    $result.containment.pointerRestored =
        $restoredPointer.X -eq $originalPointer.X -and
        $restoredPointer.Y -eq $originalPointer.Y
    $result.containment.firewallRulesRemoved =
        @(
            Get-NetFirewallRule -ErrorAction SilentlyContinue |
                Where-Object { $_.DisplayName -like "$firewallPrefix-*" }
        ).Count -eq 0
    if (
        $result.containment.remainingApplicationProcessCount -ne 0 -or
        -not $result.containment.runRootRemoved -or
        -not $result.containment.pointerRestored -or
        -not $result.containment.firewallRulesRemoved
    ) {
        $result.status = "fail"
        if ($null -eq $result.failure) {
            $result.failure = [ordered]@{
                message = "ILSpy containment cleanup failed."
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
if ($result.status -ne "pass") {
    throw $result.failure.message
}
