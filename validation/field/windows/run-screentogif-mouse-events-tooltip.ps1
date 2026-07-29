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
    [string] $ExpectedHelpText,

    [Parameter(Mandatory = $true)]
    [string] $OutputPath,

    [ValidateNotNullOrEmpty()]
    [string] $ProbeCulture = "fr-FR",

    [ValidateScript({ Test-Path -LiteralPath $_ -PathType Container })]
    [string] $DotnetRoot = "C:\lab\dotnet-campaign"
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"

Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes
Add-Type -AssemblyName System.Drawing
Add-Type -AssemblyName System.Windows.Forms
Add-Type @"
using System.Runtime.InteropServices;

public static class ReproitPointer
{
    [StructLayout(LayoutKind.Sequential)]
    public struct Point
    {
        public int X;
        public int Y;
    }

    [DllImport("user32.dll")]
    public static extern bool GetCursorPos(out Point point);

    [DllImport("user32.dll")]
    public static extern bool SetCursorPos(int x, int y);
}
"@

$applicationDirectory = (Resolve-Path -LiteralPath $ApplicationDirectory).Path
$applicationPath = Join-Path $applicationDirectory "ScreenToGif.exe"
$applicationAssemblyPath = Join-Path $applicationDirectory "ScreenToGif.dll"
$settingsPath = Join-Path $applicationDirectory "Settings.xaml"
$runRoot = Join-Path $applicationDirectory "ReproitRun"
$framePath = Join-Path $runRoot "frame.png"
$startedAt = [DateTimeOffset]::UtcNow
$stopwatch = [Diagnostics.Stopwatch]::StartNew()
$failure = $null
$originalPointer = [ReproitPointer+Point]::new()
[void][ReproitPointer]::GetCursorPos([ref]$originalPointer)

$result = [ordered]@{
    schemaVersion = 1
    campaign = "screentogif-mouse-events-tooltip-1200"
    target = "windows-wpf"
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
        probeCulture = $ProbeCulture
        automation = "Windows UI Automation"
        networkPolicy = "application update checks disabled in the portable settings"
    }
    containment = [ordered]@{
        initialProcessCount = $null
        portableSettingsAbsentBeforeRun = $null
        portableSettingsCreated = $null
        portableLanguageCode = $null
        stoppedOwnedProcessIds = @()
        remainingOwnedProcessCount = $null
        portableSettingsRemoved = $null
        runRootRemoved = $null
    }
    readiness = [ordered]@{
        processId = $null
        processSessionId = $null
        processPath = $null
        editorWindowName = $null
        imageTabName = $null
        elapsedMilliseconds = $null
    }
    observation = [ordered]@{
        expectedHelpText = $ExpectedHelpText
        observedHelpText = $null
        observedTooltipName = $null
        elementName = $null
        elementControlType = $null
        targetingMethod = $null
        minimizedTrigger =
            if ($Role -eq "control") {
                "open one frame in the French editor and read the neighboring shapes tooltip"
            } else {
                "open one frame in the French editor and read the mouse-events tooltip"
            }
        neighboringControl = $Role -eq "control"
        matchingElements = @()
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
    foreach ($ownedProcess in @(Get-OwnedProcesses)) {
        $processId = [int]$ownedProcess.ProcessId
        Stop-Process -Id $processId -Force -ErrorAction SilentlyContinue
        $stopped.Add($processId)
    }
    return @($stopped)
}

function Wait-ForApplicationProcess {
    $deadline = [DateTimeOffset]::UtcNow.AddSeconds(30)
    while ([DateTimeOffset]::UtcNow -lt $deadline) {
        $processes = @(Get-OwnedProcesses |
            Where-Object { $_.Name -eq "ScreenToGif.exe" })
        if ($processes.Count -eq 1) {
            return $processes[0]
        }
        if ($processes.Count -gt 1) {
            throw "More than one owned ScreenToGif process became ready."
        }
        Start-Sleep -Milliseconds 250
    }
    throw "ScreenToGif did not become ready within the bounded wait."
}

function Find-TooltipElement {
    param(
        [Parameter(Mandatory = $true)]
        [int] $ProcessId
    )

    $tooltipCondition = [Windows.Automation.PropertyCondition]::new(
        [Windows.Automation.AutomationElement]::ControlTypeProperty,
        [Windows.Automation.ControlType]::ToolTip
    )
    $deadline = [DateTimeOffset]::UtcNow.AddSeconds(30)
    while ([DateTimeOffset]::UtcNow -lt $deadline) {
        $elements = [Windows.Automation.AutomationElement]::RootElement.FindAll(
            [Windows.Automation.TreeScope]::Descendants,
            $tooltipCondition
        )
        $matches = [System.Collections.Generic.List[object]]::new()
        for ($index = 0; $index -lt $elements.Count; $index++) {
            $element = $elements.Item($index)
            $helpText = $element.Current.HelpText
            $name = $element.Current.Name
            $candidateProcessId = $element.Current.ProcessId
            $candidateNames = [System.Collections.Generic.List[string]]::new()
            if (-not [string]::IsNullOrWhiteSpace($name)) {
                $candidateNames.Add($name)
            }
            $children = $element.FindAll(
                [Windows.Automation.TreeScope]::Descendants,
                [Windows.Automation.Condition]::TrueCondition
            )
            for ($childIndex = 0; $childIndex -lt $children.Count; $childIndex++) {
                $childName = $children.Item($childIndex).Current.Name
                if (-not [string]::IsNullOrWhiteSpace($childName)) {
                    $candidateNames.Add($childName)
                }
            }
            $combinedName = $candidateNames -join " "
            if ($combinedName -notlike "*$ExpectedHelpText*") {
                continue
            }
            $controlType = $element.Current.ControlType.ProgrammaticName
            $matches.Add([ordered]@{
                name = $combinedName
                helpText = $helpText
                automationId = $element.Current.AutomationId
                controlType = $controlType
                className = $element.Current.ClassName
                processId = $candidateProcessId
                isEnabled = $element.Current.IsEnabled
                isOffscreen = $element.Current.IsOffscreen
            })
        }
        $exact = $matches |
            Where-Object { $_.name -like "*$ExpectedHelpText*" } |
            Select-Object -First 1
        if ($null -ne $exact) {
            return [ordered]@{
                exact = $exact
                matches = @($matches)
            }
        }
        Start-Sleep -Milliseconds 500
    }
    return [ordered]@{
        exact = $null
        matches = @($matches)
    }
}

function Find-MouseEventsTarget {
    param(
        [Parameter(Mandatory = $true)]
        [int] $ProcessId
    )

    $condition = [Windows.Automation.PropertyCondition]::new(
        [Windows.Automation.AutomationElement]::ProcessIdProperty,
        $ProcessId
    )
    $elements = [Windows.Automation.AutomationElement]::RootElement.FindAll(
        [Windows.Automation.TreeScope]::Descendants,
        $condition
    )
    $visibleElements = [System.Collections.Generic.List[object]]::new()
    for ($index = 0; $index -lt [Math]::Min($elements.Count, 2000); $index++) {
        $element = $elements.Item($index)
        $name = $element.Current.Name
        $rectangle = $element.Current.BoundingRectangle
        if (-not $rectangle.IsEmpty -and -not $element.Current.IsOffscreen) {
            if ($element.Current.ControlType.ProgrammaticName -eq
                "ControlType.Button") {
                $visibleElements.Add([ordered]@{
                    name = $name
                    automationId = $element.Current.AutomationId
                    controlType = $element.Current.ControlType.ProgrammaticName
                    className = $element.Current.ClassName
                    x = [int]$rectangle.X
                    y = [int]$rectangle.Y
                    width = [int]$rectangle.Width
                    height = [int]$rectangle.Height
                })
            }
        }
        if ($name -notmatch "MouseEvents|Mouse events|Souris|v.nements") {
            continue
        }
        if ($rectangle.IsEmpty -or $element.Current.IsOffscreen) {
            continue
        }
        return [ordered]@{
            x = [int]($rectangle.X + ($rectangle.Width / 2))
            y = [int]($rectangle.Y + ($rectangle.Height / 2))
            name = $name
            controlType = $element.Current.ControlType.ProgrammaticName
            method = "named-uia-element"
        }
    }

    $ribbon = $elements |
        Where-Object { $_.Current.AutomationId -eq "RibbonTabControl" } |
        Select-Object -First 1
    if ($null -eq $ribbon) {
        $result.observation.matchingElements = @($visibleElements)
        throw "The editor ribbon was absent from the UI Automation tree."
    }
    $ribbonRectangle = $ribbon.Current.BoundingRectangle
    if ($ribbonRectangle.IsEmpty -or $ribbon.Current.IsOffscreen) {
        $result.observation.matchingElements = @($visibleElements)
        throw "The editor ribbon had no usable screen bounds."
    }

    # ExtendedButton does not publish its content through UIA in this revision.
    # The point is derived from the fixed Image-tab grid declared in Editor.xaml:
    # localized auto-width size and text groups, then the fourth overlay button.
    # The historical editor opens at a fixed 960px width from fresh settings.
    $targetOffsetX = if ($Role -eq "control") { 494 } else { 675 }
    $targetResourceName = if ($Role -eq "control") {
        "S.Editor.Image.Shape"
    } else {
        "S.Editor.Image.MouseEvents"
    }
    return [ordered]@{
        x = [int]($ribbonRectangle.X + $targetOffsetX)
        y = [int]($ribbonRectangle.Y + 67)
        name = $targetResourceName
        controlType = "ControlType.Button"
        method = "uia-ribbon-bounds-and-fixed-xaml-layout"
    }
}

function Select-ImageTab {
    param(
        [Parameter(Mandatory = $true)]
        [int] $ProcessId
    )

    $condition = [Windows.Automation.PropertyCondition]::new(
        [Windows.Automation.AutomationElement]::ProcessIdProperty,
        $ProcessId
    )
    $deadline = [DateTimeOffset]::UtcNow.AddSeconds(30)
    while ([DateTimeOffset]::UtcNow -lt $deadline) {
        $elements = [Windows.Automation.AutomationElement]::RootElement.FindAll(
            [Windows.Automation.TreeScope]::Descendants,
            $condition
        )
        for ($index = 0; $index -lt [Math]::Min($elements.Count, 500); $index++) {
            $element = $elements.Item($index)
            if ($element.Current.ControlType.ProgrammaticName -ne
                "ControlType.TabItem") {
                continue
            }
            if ($element.Current.Name -ne "Image") {
                continue
            }
            $pattern = $element.GetCurrentPattern(
                [Windows.Automation.SelectionItemPattern]::Pattern
            )
            $pattern.Select()
            return $element.Current.Name
        }
        Start-Sleep -Milliseconds 500
    }
    throw "The Image tab was absent from the UI Automation tree."
}

function New-PortableSettings {
    $languageCode =
        [Globalization.CultureInfo]::GetCultureInfo(
            $ProbeCulture
        ).TwoLetterISOLanguageName
    $settings = @"
<ResourceDictionary xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
                    xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml"
                    xmlns:s="clr-namespace:System;assembly=mscorlib">
  <s:Boolean x:Key="CheckForUpdates">False</s:Boolean>
  <s:Boolean x:Key="CheckForTranslationUpdates">False</s:Boolean>
  <s:Boolean x:Key="SingleInstance">False</s:Boolean>
  <s:String x:Key="LanguageCode">$languageCode</s:String>
</ResourceDictionary>
"@
    Set-Content -LiteralPath $settingsPath -Value $settings -Encoding utf8
    return $languageCode
}

try {
    foreach ($requiredPath in @($applicationPath, $applicationAssemblyPath)) {
        if (-not (Test-Path -LiteralPath $requiredPath -PathType Leaf)) {
            throw "Required ScreenToGif file does not exist: $requiredPath"
        }
    }

    $result.exactIdentity.executableSha256 =
        (Get-FileHash -LiteralPath $applicationPath -Algorithm SHA256).Hash
    $result.exactIdentity.applicationAssemblySha256 =
        (Get-FileHash -LiteralPath $applicationAssemblyPath -Algorithm SHA256).Hash
    if ($result.exactIdentity.applicationAssemblySha256 -ne
        $ExpectedApplicationSha256.ToUpperInvariant()) {
        throw "ScreenToGif.dll does not match the expected revision hash."
    }
    $result.assertions += "application-hash-matches-exact-revision"

    $initialProcesses = @(Get-Process -Name "ScreenToGif" -ErrorAction SilentlyContinue)
    $result.containment.initialProcessCount = $initialProcesses.Count
    if ($initialProcesses.Count -ne 0) {
        throw "Refusing to start while any ScreenToGif process is already running."
    }
    $result.assertions += "no-preexisting-screentogif-process"

    $result.containment.portableSettingsAbsentBeforeRun =
        -not (Test-Path -LiteralPath $settingsPath)
    if (-not $result.containment.portableSettingsAbsentBeforeRun) {
        throw "Portable Settings.xaml must be absent before each run."
    }
    if (Test-Path -LiteralPath $runRoot) {
        throw "The owned run root must be absent before each run."
    }
    New-Item -ItemType Directory -Path $runRoot | Out-Null
    New-Item -ItemType Directory -Path (Join-Path $runRoot "Temp") | Out-Null
    $result.containment.portableLanguageCode = New-PortableSettings
    $result.containment.portableSettingsCreated =
        Test-Path -LiteralPath $settingsPath -PathType Leaf
    $result.assertions += "fresh-portable-settings"

    $env:DOTNET_ROOT = (Resolve-Path -LiteralPath $DotnetRoot).Path
    $env:PATH = "$env:DOTNET_ROOT;$env:PATH"
    $env:TEMP = Join-Path $runRoot "Temp"
    $env:TMP = $env:TEMP
    $frameBytes = [Convert]::FromBase64String(
        "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8" +
        "AAusB9Wl2nGQAAAAASUVORK5CYII="
    )
    [IO.File]::WriteAllBytes($framePath, $frameBytes)

    $startedProcess = Start-Process `
        -FilePath $applicationPath `
        -ArgumentList @(
            "-new",
            "-open",
            "editor",
            $framePath
        ) `
        -WorkingDirectory $applicationDirectory `
        -PassThru
    $applicationProcess = Wait-ForApplicationProcess
    $applicationProcessId = [int]$applicationProcess.ProcessId
    $result.readiness.processId = $applicationProcessId
    $result.readiness.processSessionId = [int]$applicationProcess.SessionId
    $result.readiness.processPath = $applicationProcess.ExecutablePath

    if ($result.readiness.processSessionId -ne $result.environment.sessionId) {
        throw "ScreenToGif did not start in the harness desktop session."
    }
    if (-not $result.readiness.processPath.StartsWith(
        $applicationDirectory,
        [StringComparison]::OrdinalIgnoreCase
    )) {
        throw "ScreenToGif escaped the owned application directory."
    }
    $result.assertions += "owned-process-in-interactive-session"

    $result.readiness.imageTabName =
        Select-ImageTab -ProcessId $applicationProcessId
    $result.assertions += "image-tab-selected-through-uia"
    Start-Sleep -Milliseconds 500

    $processElement = [Windows.Automation.AutomationElement]::RootElement.FindFirst(
        [Windows.Automation.TreeScope]::Descendants,
        [Windows.Automation.PropertyCondition]::new(
            [Windows.Automation.AutomationElement]::ProcessIdProperty,
            $applicationProcessId
        )
    )
    if ($null -ne $processElement) {
        $result.readiness.editorWindowName = $processElement.Current.Name
    }

    $mouseEventsTarget =
        Find-MouseEventsTarget -ProcessId $applicationProcessId
    if (-not [ReproitPointer]::SetCursorPos(
        $mouseEventsTarget.x,
        $mouseEventsTarget.y
    )) {
        throw "Failed to move the pointer over the mouse-events button."
    }
    $result.observation.elementName = $mouseEventsTarget.name
    $result.observation.elementControlType =
        $mouseEventsTarget.controlType
    $result.observation.targetingMethod = $mouseEventsTarget.method
    $result.assertions += "mouse-events-button-hovered"
    Start-Sleep -Seconds 1

    $tooltip = Find-TooltipElement -ProcessId $applicationProcessId
    $result.observation.matchingElements = @($tooltip.matches)
    if ($null -eq $tooltip.exact) {
        throw "The expected mouse-events tooltip was absent from the UIA tree."
    }
    $result.observation.observedHelpText = $tooltip.exact.helpText
    $result.observation.observedTooltipName = $tooltip.exact.name
    $result.readiness.elapsedMilliseconds = $stopwatch.ElapsedMilliseconds
    if ($result.observation.observedTooltipName -notlike
        "*$ExpectedHelpText*") {
        throw "The mouse-events tooltip did not match the expected role."
    }
    $result.assertions += "mouse-events-tooltip-matches-role"
    $result.status = "pass"
} catch {
    $failure = $_
    $result.status = "fail"
    $diagnosticScreenshotPath = "$OutputPath.png"
    try {
        $screenBounds = [Windows.Forms.SystemInformation]::VirtualScreen
        $bitmap = [Drawing.Bitmap]::new(
            $screenBounds.Width,
            $screenBounds.Height
        )
        $graphics = [Drawing.Graphics]::FromImage($bitmap)
        $graphics.CopyFromScreen(
            $screenBounds.X,
            $screenBounds.Y,
            0,
            0,
            $screenBounds.Size
        )
        $bitmap.Save(
            $diagnosticScreenshotPath,
            [Drawing.Imaging.ImageFormat]::Png
        )
        $graphics.Dispose()
        $bitmap.Dispose()
    } catch {
        $diagnosticScreenshotPath = $null
    }
    $result.failure = [ordered]@{
        message = $_.Exception.Message
        category = $_.CategoryInfo.Category.ToString()
        stack = $_.ScriptStackTrace
        diagnosticScreenshotPath = $diagnosticScreenshotPath
    }
} finally {
    [void][ReproitPointer]::SetCursorPos(
        $originalPointer.X,
        $originalPointer.Y
    )
    $result.containment.stoppedOwnedProcessIds = @(Stop-OwnedProcesses)
    Start-Sleep -Milliseconds 500
    $result.containment.remainingOwnedProcessCount =
        @(Get-OwnedProcesses).Count

    if (Test-Path -LiteralPath $settingsPath) {
        Remove-Item -LiteralPath $settingsPath -Force
    }
    $result.containment.portableSettingsRemoved =
        -not (Test-Path -LiteralPath $settingsPath)

    if (Test-Path -LiteralPath $runRoot) {
        Remove-Item -LiteralPath $runRoot -Recurse -Force
    }
    $result.containment.runRootRemoved =
        -not (Test-Path -LiteralPath $runRoot)

    if ($result.containment.remainingOwnedProcessCount -ne 0 -or
        -not $result.containment.portableSettingsRemoved -or
        -not $result.containment.runRootRemoved) {
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
