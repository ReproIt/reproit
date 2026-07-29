[CmdletBinding()]
param(
    [ValidateScript({ Test-Path -LiteralPath $_ -PathType Container })]
    [string] $CampaignRoot = "C:\lab\campaigns\avalonia",

    [string] $OutputDirectory = "C:\lab\campaigns\avalonia\evidence"
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"

$picViewHarness = Join-Path $CampaignRoot "run-picview-corner-close.ps1"
$ilSpyHarness = Join-Path $CampaignRoot "run-ilspy-fold-documentation.ps1"
$subjectAssembly = Join-Path $CampaignRoot "ilspy-subject\publish\Subject.dll"

foreach ($requiredPath in @(
    $picViewHarness,
    $ilSpyHarness,
    $subjectAssembly
)) {
    if (-not (Test-Path -LiteralPath $requiredPath -PathType Leaf)) {
        throw "Required Avalonia campaign path is missing: $requiredPath"
    }
}
New-Item -ItemType Directory -Force -Path $OutputDirectory | Out-Null

$campaigns = @(
    [ordered]@{
        id = "picview-corner"
        harness = $picViewHarness
        affectedDirectory = Join-Path $CampaignRoot "corner-affected-publish"
        affectedRevision = "fd7acc2535ef8b2e7edeeb9d6b8507f09e3b411c"
        affectedSha256 =
            "E47D6F039328B65EBC00463E1CF0395E65AF9792758D40EE688D09B599022AA7"
        fixedDirectory = Join-Path $CampaignRoot "corner-fixed-publish"
        fixedRevision = "00cd32fdcc2332fc48ba1465e600b852ca09ee25"
        fixedSha256 =
            "62C88246FD5FF0F3863917AF093D2D0B80F9A1E361BB42F28F70E1D90E52DD69"
        controlDirectory = Join-Path $CampaignRoot "corner-affected-publish"
        controlRevision = "fd7acc2535ef8b2e7edeeb9d6b8507f09e3b411c"
        controlSha256 =
            "E47D6F039328B65EBC00463E1CF0395E65AF9792758D40EE688D09B599022AA7"
        extraArguments = @{}
    },
    [ordered]@{
        id = "ilspy"
        harness = $ilSpyHarness
        affectedDirectory = Join-Path $CampaignRoot "ilspy-affected-publish"
        affectedRevision = "48fb85960e2adce0367ba925d3f2bf1f6b0384f9"
        affectedSha256 =
            "37F8AEB783E4DF03CA4B331B8DB6265CADEABF087EA20553A714A989B029BEF3"
        fixedDirectory = Join-Path $CampaignRoot "ilspy-fixed-publish"
        fixedRevision = "800efc6e105ce4a94f25a335938c53927f3cb4b6"
        fixedSha256 =
            "7C3CEF37728D8FE7D1B27ED681E47BA714264FAE0E1466E7810D30D1ED1BFDFB"
        controlDirectory = Join-Path $CampaignRoot "ilspy-affected-publish"
        controlRevision = "48fb85960e2adce0367ba925d3f2bf1f6b0384f9"
        controlSha256 =
            "37F8AEB783E4DF03CA4B331B8DB6265CADEABF087EA20553A714A989B029BEF3"
        extraArguments = @{
            SubjectAssembly = $subjectAssembly
            ExpectedSubjectSha256 =
                "6BEB927C194CDF895F32346720FA218B001DC594E744702F349F7A9DBB7CCCF9"
        }
    }
)

$records = [System.Collections.Generic.List[object]]::new()

function Invoke-Role {
    param(
        [Collections.IDictionary] $Campaign,
        [ValidateSet("affected", "fixed", "control")]
        [string] $Role,
        [int] $Run
    )

    $directory = $Campaign["${Role}Directory"]
    $revision = $Campaign["${Role}Revision"]
    $sha256 = $Campaign["${Role}Sha256"]
    $outputPath = Join-Path $OutputDirectory (
        "$($Campaign.id)-$Role-$Run.json"
    )
    $arguments = @{
        ApplicationDirectory = $directory
        ExpectedRevision = $revision
        ExpectedApplicationSha256 = $sha256
        Role = $Role
        Run = $Run
        OutputPath = $outputPath
    }
    foreach ($entry in $Campaign.extraArguments.GetEnumerator()) {
        $arguments[$entry.Key] = $entry.Value
    }
    & $Campaign.harness @arguments

    $record = Get-Content -LiteralPath $outputPath -Raw | ConvertFrom-Json
    if ($record.status -ne "pass") {
        throw "$($Campaign.id) $Role run $Run did not pass."
    }
    $records.Add([ordered]@{
        campaign = $record.campaign
        role = $Role
        run = $Run
        revision = $revision
        elapsedSeconds = $record.elapsedSeconds
        rawRecordSha256 =
            (Get-FileHash -LiteralPath $outputPath -Algorithm SHA256).Hash
        outputPath = $outputPath
    })
}

foreach ($campaign in $campaigns) {
    foreach ($run in 1..3) {
        Invoke-Role -Campaign $campaign -Role "affected" -Run $run
    }
    foreach ($run in 1..3) {
        Invoke-Role -Campaign $campaign -Role "fixed" -Run $run
    }
    Invoke-Role -Campaign $campaign -Role "control" -Run 1
}

$remainingProcesses = @(
    Get-Process -Name "PicView", "ILSpy" -ErrorAction SilentlyContinue
).Count
$remainingRules = @(
    Get-NetFirewallRule -ErrorAction SilentlyContinue |
        Where-Object { $_.DisplayName -like "ReproitAvalonia-*" }
).Count
if ($remainingProcesses -ne 0 -or $remainingRules -ne 0) {
    throw "The Avalonia campaign left owned process or firewall state behind."
}

$summaryPath = Join-Path $OutputDirectory "windows-avalonia-campaign-summary.json"
[ordered]@{
    schemaVersion = 1
    target = "windows-avalonia"
    status = "pass"
    records = @($records)
    cleanup = [ordered]@{
        remainingApplicationProcesses = $remainingProcesses
        remainingFirewallRules = $remainingRules
    }
} |
    ConvertTo-Json -Depth 8 |
    Set-Content -LiteralPath $summaryPath -Encoding utf8

Write-Host "Windows Avalonia campaigns passed PicView and ILSpy."
