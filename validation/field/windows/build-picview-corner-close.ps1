[CmdletBinding()]
param(
    [ValidateScript({ Test-Path -LiteralPath $_ -PathType Container })]
    [string] $CampaignRoot = "C:\lab\campaigns\avalonia",

    [ValidateScript({ Test-Path -LiteralPath $_ -PathType Container })]
    [string] $DotnetRoot = "C:\lab\dotnet11"
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"

$sourceRepository = Join-Path $CampaignRoot "picview-source"
$projectRelativePath =
    "src\PicView.Avalonia.Win32\PicView.Avalonia.Win32.csproj"
$builds = @(
    [ordered]@{
        role = "affected"
        revision = "fd7acc2535ef8b2e7edeeb9d6b8507f09e3b411c"
    },
    [ordered]@{
        role = "fixed"
        revision = "00cd32fdcc2332fc48ba1465e600b852ca09ee25"
    }
)

if (@(Get-Process -Name "PicView" -ErrorAction SilentlyContinue).Count -ne 0) {
    throw "Refusing to build while PicView is running."
}

$env:DOTNET_ROOT = (Resolve-Path -LiteralPath $DotnetRoot).Path
$env:PATH = "$env:DOTNET_ROOT;$env:PATH"

foreach ($build in $builds) {
    $revision = $build.revision
    & git -C $sourceRepository cat-file -e "$revision`^{commit}"
    if ($LASTEXITCODE -ne 0) {
        throw "PicView revision is absent: $revision"
    }

    $worktree = Join-Path $CampaignRoot "corner-$($build.role)"
    $publishDirectory =
        Join-Path $CampaignRoot "corner-$($build.role)-publish"
    $logPath = Join-Path $CampaignRoot "corner-$($build.role)-build.log"

    if (Test-Path -LiteralPath $worktree) {
        & git -C $sourceRepository worktree remove --force $worktree
        if ($LASTEXITCODE -ne 0) {
            throw "Could not remove the existing $($build.role) worktree."
        }
    }
    Remove-Item `
        -LiteralPath $publishDirectory, $logPath `
        -Recurse `
        -Force `
        -ErrorAction SilentlyContinue

    & git -C $sourceRepository worktree add --detach $worktree $revision
    if ($LASTEXITCODE -ne 0) {
        throw "Could not create the $($build.role) worktree."
    }
    $actualRevision = (& git -C $worktree rev-parse HEAD).Trim()
    if ($actualRevision -ne $revision) {
        throw "The $($build.role) worktree has the wrong revision."
    }

    $projectPath = Join-Path $worktree $projectRelativePath
    & (Join-Path $DotnetRoot "dotnet.exe") publish `
        $projectPath `
        --configuration Release `
        --runtime win-x64 `
        --self-contained false `
        --output $publishDirectory `
        -p:PublishAot=false `
        -p:PublishReadyToRun=false *> $logPath
    if ($LASTEXITCODE -ne 0) {
        throw "The $($build.role) native PicView publish failed."
    }

    $applicationPath = Join-Path $publishDirectory "PicView.exe"
    $assemblyPath = Join-Path $publishDirectory "PicView.dll"
    foreach ($path in @($applicationPath, $assemblyPath)) {
        if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
            throw "The publish omitted required artifact: $path"
        }
    }

    [ordered]@{
        role = $build.role
        revision = $actualRevision
        executableSha256 =
            (Get-FileHash $applicationPath -Algorithm SHA256).Hash
        applicationAssemblySha256 =
            (Get-FileHash $assemblyPath -Algorithm SHA256).Hash
        buildLogSha256 = (Get-FileHash $logPath -Algorithm SHA256).Hash
        publishDirectory = $publishDirectory
    } |
        ConvertTo-Json -Depth 4 |
        Set-Content `
            -LiteralPath (
                Join-Path $CampaignRoot "corner-$($build.role)-build.json"
            ) `
            -Encoding utf8
}
