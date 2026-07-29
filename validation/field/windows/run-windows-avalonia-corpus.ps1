[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateSet(
        "picview-fixed-clean",
        "picview-affected-close-center",
        "ilspy-affected-documentation-fold"
    )]
    [string] $CaseId,

    [ValidateScript({ Test-Path -LiteralPath $_ -PathType Container })]
    [string] $CampaignRoot = "C:\lab\campaigns\avalonia",

    [string] $OutputDirectory = "C:\lab\campaigns\avalonia\corpus"
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"

New-Item -ItemType Directory -Force -Path $OutputDirectory | Out-Null
$outputPath = Join-Path $OutputDirectory "$CaseId.json"

switch ($CaseId) {
    "picview-fixed-clean" {
        & (Join-Path $CampaignRoot "run-picview-corner-close.ps1") `
            -ApplicationDirectory (
                Join-Path $CampaignRoot "corner-fixed-publish"
            ) `
            -ExpectedRevision "00cd32fdcc2332fc48ba1465e600b852ca09ee25" `
            -ExpectedApplicationSha256 (
                "62C88246FD5FF0F3863917AF093D2D0B80F9A1E361BB42F28F70E1D90E52DD69"
            ) `
            -Role "fixed" `
            -Run 1 `
            -OutputPath $outputPath
    }
    "picview-affected-close-center" {
        & (Join-Path $CampaignRoot "run-picview-corner-close.ps1") `
            -ApplicationDirectory (
                Join-Path $CampaignRoot "corner-affected-publish"
            ) `
            -ExpectedRevision "fd7acc2535ef8b2e7edeeb9d6b8507f09e3b411c" `
            -ExpectedApplicationSha256 (
                "E47D6F039328B65EBC00463E1CF0395E65AF9792758D40EE688D09B599022AA7"
            ) `
            -Role "control" `
            -Run 1 `
            -OutputPath $outputPath
    }
    "ilspy-affected-documentation-fold" {
        & (Join-Path $CampaignRoot "run-ilspy-fold-documentation.ps1") `
            -ApplicationDirectory (
                Join-Path $CampaignRoot "ilspy-affected-publish"
            ) `
            -ExpectedRevision "48fb85960e2adce0367ba925d3f2bf1f6b0384f9" `
            -ExpectedApplicationSha256 (
                "37F8AEB783E4DF03CA4B331B8DB6265CADEABF087EA20553A714A989B029BEF3"
            ) `
            -SubjectAssembly (
                Join-Path $CampaignRoot "ilspy-subject\publish\Subject.dll"
            ) `
            -ExpectedSubjectSha256 (
                "6BEB927C194CDF895F32346720FA218B001DC594E744702F349F7A9DBB7CCCF9"
            ) `
            -Role "control" `
            -Run 1 `
            -OutputPath $outputPath
    }
}

$record = Get-Content -LiteralPath $outputPath -Raw | ConvertFrom-Json
if ($record.status -ne "pass") {
    throw "Avalonia corpus case $CaseId did not pass."
}
if (
    -not $record.containment.inboundFirewallRuleActive -or
    -not $record.containment.outboundFirewallRuleActive -or
    -not $record.containment.firewallRulesRemoved -or
    -not $record.containment.runRootRemoved
) {
    throw "Avalonia corpus case $CaseId was not contained and cleaned."
}

Write-Host "Windows Avalonia corpus case passed: $CaseId"
