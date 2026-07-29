[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateSet(
        "flow-fixed-fr-clean",
        "flow-affected-en-adversarial",
        "screentogif-affected-shapes-adversarial"
    )]
    [string] $CaseId,

    [Parameter(Mandatory = $true)]
    [string] $OutputPath,

    [ValidateScript({ Test-Path -LiteralPath $_ -PathType Container })]
    [string] $CampaignRoot = "C:\lab\campaigns"
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"

$flowHarness = Join-Path $CampaignRoot "run-flowlauncher-system-language.ps1"
$screenToGifHarness =
    Join-Path $CampaignRoot "run-screentogif-mouse-events-tooltip.ps1"
$firewallPrefix = "ReproitWpfCorpus-$CaseId"
$applicationPath = $null
$harnessPath = $null
$harnessArguments = @{}

switch ($CaseId) {
    "flow-fixed-fr-clean" {
        $applicationDirectory = Join-Path $CampaignRoot (
            "flowlauncher-builds\flowlauncher-system-language-4518-fixed"
        )
        $applicationPath = Join-Path $applicationDirectory "Flow.Launcher.exe"
        $harnessPath = $flowHarness
        $harnessArguments = @{
            ApplicationDirectory = $applicationDirectory
            ExpectedRevision = "eb3da1edb942983c14b7c78a4d50dee368c23002"
            ExpectedCoreSha256 =
                "1C0A5EA7F8C2BD3486E51B2BD93A1B5BC60539A4BBF6BF8AE45FD7AB797E3621"
            Role = "fixed"
            Run = 1
            OutputPath = $OutputPath
            ProbeCulture = "fr-FR"
            ExpectedTitle = "Bienvenue dans Flow Launcher"
        }
    }
    "flow-affected-en-adversarial" {
        $applicationDirectory = Join-Path $CampaignRoot (
            "flowlauncher-builds\flowlauncher-system-language-4518-affected"
        )
        $applicationPath = Join-Path $applicationDirectory "Flow.Launcher.exe"
        $harnessPath = $flowHarness
        $harnessArguments = @{
            ApplicationDirectory = $applicationDirectory
            ExpectedRevision = "447fbea4882b7d1df9bf6c510752dc236a30a115"
            ExpectedCoreSha256 =
                "0A8A33423B68EED22F494B06CFC51A3C236AA31372800CABE7219878CF58730C"
            Role = "affected-control"
            Run = 1
            OutputPath = $OutputPath
            ProbeCulture = "en-US"
            ExpectedTitle = "Welcome to Flow Launcher"
        }
    }
    "screentogif-affected-shapes-adversarial" {
        $applicationDirectory = Join-Path $CampaignRoot (
            "screentogif-builds\screentogif-mouse-events-tooltip-1200-affected"
        )
        $applicationPath = Join-Path $applicationDirectory "ScreenToGif.exe"
        $harnessPath = $screenToGifHarness
        $harnessArguments = @{
            ApplicationDirectory = $applicationDirectory
            ExpectedRevision = "e44f1f1ef0086fcb3cd85d55156e94a2957e3dd1"
            ExpectedApplicationSha256 =
                "88EB418E734DED16AB711F19CC801336DF27AD02B286111AC145D9C72B6A4405"
            Role = "control"
            Run = 1
            ExpectedHelpText = "Ajouter des formes"
            OutputPath = $OutputPath
            ProbeCulture = "fr-FR"
        }
    }
}

foreach ($requiredPath in @($applicationPath, $harnessPath)) {
    if (-not (Test-Path -LiteralPath $requiredPath -PathType Leaf)) {
        throw "Required corpus path does not exist: $requiredPath"
    }
}

$rules = [System.Collections.Generic.List[string]]::new()
try {
    foreach ($direction in @("Inbound", "Outbound")) {
        $ruleName = "$firewallPrefix-$direction"
        New-NetFirewallRule `
            -DisplayName $ruleName `
            -Direction $direction `
            -Action Block `
            -Enabled True `
            -Profile Any `
            -Program $applicationPath | Out-Null
        $rules.Add($ruleName)
    }

    $activeRules = @(
        Get-NetFirewallRule |
            Where-Object {
                $_.DisplayName -like "$firewallPrefix-*" -and
                $_.Enabled -eq "True" -and
                $_.Action -eq "Block"
            }
    )
    if ($activeRules.Count -ne 2) {
        throw "The subject did not receive both offline firewall rules."
    }

    & $harnessPath @harnessArguments

    $record = Get-Content -Raw -LiteralPath $OutputPath | ConvertFrom-Json
    if ($record.status -ne "pass") {
        throw "The native WPF corpus observation did not pass."
    }
    $record.environment.networkPolicy =
        "none: program-scoped Windows Firewall inbound and outbound block rules"
    $record | Add-Member -NotePropertyName offlineContainment -NotePropertyValue (
        [ordered]@{
            applicationPath = $applicationPath
            inboundRuleActiveAtLaunch = $true
            outboundRuleActiveAtLaunch = $true
            profile = "Any"
        }
    )
    $record |
        ConvertTo-Json -Depth 12 |
        Set-Content -LiteralPath $OutputPath -Encoding utf8
} catch {
    [ordered]@{
        message = $_.Exception.Message
        category = $_.CategoryInfo.Category.ToString()
        stack = $_.ScriptStackTrace
    } |
        ConvertTo-Json -Depth 4 |
        Set-Content -LiteralPath "$OutputPath.wrapper-error.json" -Encoding utf8
    throw
} finally {
    foreach ($ruleName in $rules) {
        Remove-NetFirewallRule -DisplayName $ruleName -ErrorAction SilentlyContinue
    }
}

$remainingRules = @(
    Get-NetFirewallRule -ErrorAction SilentlyContinue |
        Where-Object { $_.DisplayName -like "$firewallPrefix-*" }
)
if ($remainingRules.Count -ne 0) {
    throw "An owned offline firewall rule survived the corpus case."
}
