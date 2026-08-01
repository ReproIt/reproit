[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateSet(
        "dlss829-fixed-minimize-clean",
        "dlss829-affected-normal-close-adversarial",
        "unigetui-affected-unbound-key-adversarial"
    )]
    [string] $CaseId,

    [Parameter(Mandatory = $true)]
    [string] $OutputPath,

    [ValidateScript({ Test-Path -LiteralPath $_ -PathType Container })]
    [string] $CampaignRoot = "C:\lab\interactive"
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"

$dlssHarness = Join-Path $CampaignRoot "run-dlss-swapper-window-position.ps1"
$uniGetUiHarness = Join-Path $CampaignRoot "run-unigetui-mainview-keyup.ps1"
$publishRoot = "C:\lab\campaigns\winui"
$firewallPrefix = "ReproitWinuiCorpus-$CaseId"
$applicationPath = $null
$harnessPath = $null
$harnessArguments = @{}

switch ($CaseId) {
    "dlss829-fixed-minimize-clean" {
        $applicationDirectory = Join-Path $publishRoot "dlss829-fixed-publish"
        $applicationPath = Join-Path $applicationDirectory "DLSS Swapper.exe"
        $harnessPath = $dlssHarness
        $harnessArguments = @{
            ApplicationDirectory = $applicationDirectory
            ExpectedRevision = "09bfaa388cbab84cf8f416576b6e401d64f98e10"
            ExpectedAssemblySha256 =
                "115A5F9EB936467F6771AE2D72A9D90BD4CF5BE4598D8FFB49384001863C69FD"
            Role = "fixed"
            Run = 1
            OutputPath = $OutputPath
        }
    }
    "dlss829-affected-normal-close-adversarial" {
        $applicationDirectory = Join-Path $publishRoot "dlss829-affected-publish"
        $applicationPath = Join-Path $applicationDirectory "DLSS Swapper.exe"
        $harnessPath = $dlssHarness
        $harnessArguments = @{
            ApplicationDirectory = $applicationDirectory
            ExpectedRevision = "8cd0ccccce1470844d4e3f9294eecbbad3c25672"
            ExpectedAssemblySha256 =
                "16339D953C334A892B1EC44BFF4A933F51E931CE96971725EFB21952FACE4E8D"
            Role = "affected-control"
            Run = 1
            OutputPath = $OutputPath
        }
    }
    "unigetui-affected-unbound-key-adversarial" {
        $applicationDirectory = Join-Path $publishRoot "unigetui-affected-publish"
        $applicationPath = Join-Path $applicationDirectory "UniGetUI.exe"
        $harnessPath = $uniGetUiHarness
        $harnessArguments = @{
            ApplicationDirectory = $applicationDirectory
            ExpectedRevision = "fecaf3d46cfdb32ed396a2e903a29d6dec40c2b5"
            ExpectedAssemblySha256 =
                "C5FA1F61EBEFDDDDFF8452DA096D98825D90DCD5D89E3AAA5D39C99732639A97"
            Role = "affected-control"
            Run = 1
            OutputPath = $OutputPath
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
        throw "The native WinUI corpus observation did not pass."
    }
    if ($null -ne $record.observation.identity) {
        throw "A known-good corpus subject reported a failure identity."
    }
    if (-not $record.observation.observationReached) {
        throw "The corpus subject never reached its observation point."
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
