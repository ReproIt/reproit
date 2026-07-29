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
    [string] $ExpectedCoreSha256,

    [Parameter(Mandatory = $true)]
    [ValidateSet("affected", "fixed", "affected-control", "fixed-control")]
    [string] $Role,

    [Parameter(Mandatory = $true)]
    [ValidateRange(1, 3)]
    [int] $Run,

    [Parameter(Mandatory = $true)]
    [string] $OutputPath,

    [ValidateNotNullOrEmpty()]
    [string] $ProbeCulture = "fr-FR",

    [ValidateNotNullOrEmpty()]
    [string] $ExpectedTitle = "Bienvenue dans Flow Launcher",

    [ValidateScript({ Test-Path -LiteralPath $_ -PathType Container })]
    [string] $DotnetRoot = "C:\lab\dotnet-campaign"
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"

Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes

$applicationDirectory = (Resolve-Path -LiteralPath $ApplicationDirectory).Path
$applicationPath = Join-Path $applicationDirectory "Flow.Launcher.exe"
$corePath = Join-Path $applicationDirectory "Flow.Launcher.Core.dll"
$userDataPath = Join-Path $applicationDirectory "UserData"
$settingsPath = Join-Path $userDataPath "Settings\Settings.json"
$originalCulture = (Get-Culture).Name
$startedAt = [DateTimeOffset]::UtcNow
$stopwatch = [Diagnostics.Stopwatch]::StartNew()
$ownedProcessIds = [System.Collections.Generic.List[int]]::new()
$failure = $null

$result = [ordered]@{
    schemaVersion = 1
    campaign = "flowlauncher-system-language-4518"
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
        coreSha256 = $null
    }
    environment = [ordered]@{
        os = [Environment]::OSVersion.VersionString
        architecture =
            [Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString()
        sessionId = (Get-Process -Id $PID).SessionId
        originalCulture = $originalCulture
        probeCulture = $ProbeCulture
        registryCultureAfterSet = $null
        automation = "Windows UI Automation"
        networkPolicy =
            "not isolated; the launch and UI Automation read require no network"
    }
    containment = [ordered]@{
        initialFlowProcessCount = $null
        userDataAbsentBeforeRun = $null
        settingsLanguage = $null
        userDataFileCountBeforeCleanup = $null
        stoppedOwnedProcessIds = @()
        remainingOwnedProcessCount = $null
        userDataRemoved = $null
        cultureRestored = $null
    }
    readiness = [ordered]@{
        processId = $null
        processSessionId = $null
        processPath = $null
        welcomeWindowAutomationId = $null
        elapsedMilliseconds = $null
    }
    observation = [ordered]@{
        expectedTitle = $ExpectedTitle
        observedTitle = $null
        titleAutomationId = "TitleTextBlock"
        minimizedTrigger =
            "launch with a fresh portable UserData directory under the selected culture"
        neighboringControl = $Role.EndsWith("-control")
        elements = @()
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
            Where-Object { $_.Name -eq "Flow.Launcher.exe" })
        if ($processes.Count -eq 1) {
            return $processes[0]
        }
        if ($processes.Count -gt 1) {
            throw "More than one owned Flow Launcher process became ready."
        }
        Start-Sleep -Milliseconds 250
    }
    throw "Flow Launcher did not become ready within the bounded wait."
}

function Wait-ForWelcomeWindow {
    param(
        [Parameter(Mandatory = $true)]
        [int] $ProcessId
    )

    $processCondition = [Windows.Automation.PropertyCondition]::new(
        [Windows.Automation.AutomationElement]::ProcessIdProperty,
        $ProcessId
    )
    $idCondition = [Windows.Automation.PropertyCondition]::new(
        [Windows.Automation.AutomationElement]::AutomationIdProperty,
        "FlowWelcomeWindow"
    )
    $condition = [Windows.Automation.AndCondition]::new(
        $processCondition,
        $idCondition
    )
    $deadline = [DateTimeOffset]::UtcNow.AddSeconds(30)
    while ([DateTimeOffset]::UtcNow -lt $deadline) {
        $window = [Windows.Automation.AutomationElement]::RootElement.FindFirst(
            [Windows.Automation.TreeScope]::Descendants,
            $condition
        )
        if ($null -ne $window) {
            return $window
        }
        Start-Sleep -Milliseconds 250
    }
    throw "The Flow Launcher welcome window did not enter the UIA tree."
}

function Get-UiaSnapshot {
    param(
        [Parameter(Mandatory = $true)]
        [Windows.Automation.AutomationElement] $Window
    )

    $elements = $Window.FindAll(
        [Windows.Automation.TreeScope]::Subtree,
        [Windows.Automation.Condition]::TrueCondition
    )
    $limit = [Math]::Min($elements.Count, 100)
    $snapshot = [System.Collections.Generic.List[object]]::new()
    for ($index = 0; $index -lt $limit; $index++) {
        $element = $elements.Item($index)
        $snapshot.Add([ordered]@{
            name = $element.Current.Name
            automationId = $element.Current.AutomationId
            controlType = $element.Current.ControlType.ProgrammaticName
            className = $element.Current.ClassName
            isEnabled = $element.Current.IsEnabled
            isOffscreen = $element.Current.IsOffscreen
        })
    }
    return @($snapshot)
}

function Get-SettingsLanguage {
    if (-not (Test-Path -LiteralPath $settingsPath -PathType Leaf)) {
        return $null
    }
    $settings = Get-Content -Raw -LiteralPath $settingsPath
    $match = [regex]::Match(
        $settings,
        '"Language"\s*:\s*"(?<value>[^"]+)"'
    )
    if (-not $match.Success) {
        return $null
    }
    return $match.Groups["value"].Value
}

try {
    foreach ($requiredPath in @($applicationPath, $corePath)) {
        if (-not (Test-Path -LiteralPath $requiredPath -PathType Leaf)) {
            throw "Required Flow Launcher file does not exist: $requiredPath"
        }
    }

    $result.exactIdentity.executableSha256 =
        (Get-FileHash -LiteralPath $applicationPath -Algorithm SHA256).Hash
    $result.exactIdentity.coreSha256 =
        (Get-FileHash -LiteralPath $corePath -Algorithm SHA256).Hash
    if ($result.exactIdentity.coreSha256 -ne $ExpectedCoreSha256.ToUpperInvariant()) {
        throw "Flow.Launcher.Core.dll does not match the expected revision hash."
    }
    $result.assertions += "core-hash-matches-exact-revision"

    $initialProcesses = @(Get-Process -Name "Flow.Launcher" -ErrorAction SilentlyContinue)
    $result.containment.initialFlowProcessCount = $initialProcesses.Count
    if ($initialProcesses.Count -ne 0) {
        throw "Refusing to start while any Flow Launcher process is already running."
    }
    $result.assertions += "no-preexisting-flow-launcher-process"

    $result.containment.userDataAbsentBeforeRun =
        -not (Test-Path -LiteralPath $userDataPath)
    if (-not $result.containment.userDataAbsentBeforeRun) {
        throw "Portable UserData must be absent before each campaign run."
    }
    New-Item -ItemType Directory -Path $userDataPath | Out-Null
    $result.assertions += "fresh-portable-user-data"

    $env:DOTNET_ROOT = (Resolve-Path -LiteralPath $DotnetRoot).Path
    $env:PATH = "$env:DOTNET_ROOT;$env:PATH"
    Set-Culture -CultureInfo $ProbeCulture
    $result.environment.registryCultureAfterSet =
        (Get-ItemProperty "HKCU:\Control Panel\International").LocaleName
    if ($result.environment.registryCultureAfterSet -ne $ProbeCulture) {
        throw "Set-Culture did not update the current user locale."
    }
    $result.assertions += "probe-culture-configured"

    $startedProcess = Start-Process `
        -FilePath $applicationPath `
        -WorkingDirectory $applicationDirectory `
        -PassThru
    $ownedProcessIds.Add($startedProcess.Id)

    $applicationProcess = Wait-ForApplicationProcess
    $applicationProcessId = [int]$applicationProcess.ProcessId
    if (-not $ownedProcessIds.Contains($applicationProcessId)) {
        $ownedProcessIds.Add($applicationProcessId)
    }
    $result.readiness.processId = $applicationProcessId
    $result.readiness.processSessionId = [int]$applicationProcess.SessionId
    $result.readiness.processPath = $applicationProcess.ExecutablePath

    if ($result.readiness.processSessionId -ne $result.environment.sessionId) {
        throw "Flow Launcher did not start in the harness desktop session."
    }
    if (-not $result.readiness.processPath.StartsWith(
        $applicationDirectory,
        [StringComparison]::OrdinalIgnoreCase
    )) {
        throw "Flow Launcher escaped the owned application directory."
    }
    $result.assertions += "owned-process-in-interactive-session"

    $welcomeWindow = Wait-ForWelcomeWindow -ProcessId $applicationProcessId
    $result.readiness.welcomeWindowAutomationId =
        $welcomeWindow.Current.AutomationId
    $result.readiness.elapsedMilliseconds = $stopwatch.ElapsedMilliseconds
    $result.assertions += "uia-welcome-window-ready"

    $snapshot = @(Get-UiaSnapshot -Window $welcomeWindow)
    $result.observation.elements = $snapshot
    $title = $snapshot |
        Where-Object { $_.automationId -eq "TitleTextBlock" } |
        Select-Object -First 1
    if ($null -eq $title) {
        throw "The localized title was absent from the UI Automation tree."
    }
    $result.observation.observedTitle = $title.name
    if ($result.observation.observedTitle -ne $ExpectedTitle) {
        throw (
            "Expected UIA title '$ExpectedTitle', observed " +
            "'$($result.observation.observedTitle)'."
        )
    }
    $result.assertions += "localized-uia-title-matches-role"

    $result.containment.settingsLanguage = Get-SettingsLanguage
    if ($result.containment.settingsLanguage -ne "system") {
        throw "The generated portable profile did not keep Language='system'."
    }
    $result.assertions += "portable-settings-language-is-system"
    $result.containment.userDataFileCountBeforeCleanup =
        @(Get-ChildItem -LiteralPath $userDataPath -Recurse -File).Count
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
    Start-Sleep -Milliseconds 500
    $result.containment.remainingOwnedProcessCount =
        @(Get-OwnedProcesses).Count

    if (Test-Path -LiteralPath $userDataPath) {
        Remove-Item -LiteralPath $userDataPath -Recurse -Force
    }
    $result.containment.userDataRemoved =
        -not (Test-Path -LiteralPath $userDataPath)

    Set-Culture -CultureInfo $originalCulture
    $restoredRegistryCulture =
        (Get-ItemProperty "HKCU:\Control Panel\International").LocaleName
    $result.containment.cultureRestored =
        $restoredRegistryCulture -eq $originalCulture

    if ($result.containment.remainingOwnedProcessCount -ne 0 -or
        -not $result.containment.userDataRemoved -or
        -not $result.containment.cultureRestored) {
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
