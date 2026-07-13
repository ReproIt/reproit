$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"
Set-StrictMode -Version Latest

$root = (Resolve-Path (Join-Path $PSScriptRoot "../..")).Path
$out = Join-Path $env:TEMP "reproit-windows-backends"
$fuzz = Join-Path $out "fuzz.json"
$config = Join-Path $out "reproit.yaml"

if ([Environment]::OSVersion.Platform -ne [PlatformID]::Win32NT) {
    throw "Windows desktop backend gate must run on Windows"
}
New-Item -ItemType Directory -Force -Path $out | Out-Null
'{"budget":4}' | Set-Content -Encoding Ascii $fuzz
'value_nodes: []' | Set-Content -Encoding Ascii $config
foreach ($name in @("WpfFixture", "AvaloniaFixture", "WinUiFixture")) {
    Stop-Process -Name $name -Force -ErrorAction SilentlyContinue
}
Start-Sleep -Milliseconds 500

Push-Location $root
try {
    $env:CARGO_TARGET_DIR = Join-Path $env:TEMP "reproit-backend-target"
    cargo clean -p reproit --release
    cargo build -p reproit --release
    $runner = Join-Path $env:CARGO_TARGET_DIR "release/reproit.exe"

    $fixtures = @(
        @{ Name = "WPF"; Project = "examples/wpf-fixture/WpfFixture.csproj"; Exe = "WpfFixture.exe"; Process = "WpfFixture" },
        @{ Name = "Avalonia"; Project = "examples/avalonia-fixture/AvaloniaFixture.csproj"; Exe = "AvaloniaFixture.exe"; Process = "AvaloniaFixture" },
        @{ Name = "WinUI"; Project = "examples/winui-fixture/WinUiFixture.csproj"; Exe = "WinUiFixture.exe"; Process = "WinUiFixture" }
    )

    foreach ($fixture in $fixtures) {
        $publish = Join-Path $out $fixture.Name
        Remove-Item -Recurse -Force $publish -ErrorAction SilentlyContinue
        $args = @("publish", $fixture.Project, "-c", "Release", "-r", "win-x64", "--self-contained", "false", "-o", $publish)
        if ($fixture.Name -eq "WinUI") { $args += "-p:Platform=x64" }
        & dotnet @args
        if ($LASTEXITCODE -ne 0) { throw "$($fixture.Name) publish failed" }

        $target = Join-Path $publish $fixture.Exe
        $log = Join-Path $out "$($fixture.Name).log"
        $runOut = Join-Path $out "$($fixture.Name).stdout.log"
        $runErr = Join-Path $out "$($fixture.Name).stderr.log"
        Remove-Item $log, $runOut, $runErr -Force -ErrorAction SilentlyContinue
        $env:REPROIT_TARGET = $target
        $env:REPROIT_FUZZ_BUDGET = "4"
        $env:REPROIT_CONFIG = $config
        $env:REPROIT_DEVICE = "a"
        try {
            $run = Start-Process -FilePath $runner -ArgumentList "__uia" `
                -RedirectStandardOutput $runOut -RedirectStandardError $runErr -PassThru
            $finished = $run.WaitForExit(90000)
            if (-not $finished) {
                Stop-Process -Id $run.Id -Force -ErrorAction SilentlyContinue
                throw "$($fixture.Name) UIA runner timed out after 90 seconds"
            }
            # PowerShell can leave ExitCode unpopulated after the timed .NET
            # WaitForExit overload until the process object is refreshed.
            $run.WaitForExit()
            $run.Refresh()
            $exitCode = $run.ExitCode
            $stdoutText = if (Test-Path $runOut) { Get-Content -Raw $runOut } else { "" }
            $stderrText = if (Test-Path $runErr) { Get-Content -Raw $runErr } else { "" }
            [System.IO.File]::WriteAllText($log, $stdoutText + $stderrText)
            Write-Host $stdoutText
            if ($stderrText) { Write-Warning $stderrText }
            if ($null -eq $exitCode) { throw "$($fixture.Name) UIA runner exit code was unavailable" }
            if ($exitCode -ne 0) { throw "$($fixture.Name) UIA runner exited $exitCode" }
            $text = Get-Content -Raw $log
            foreach ($marker in @("EXPLORE:STATE", "EXPLORE:EDGE", "JOURNEY DONE", "All tests passed")) {
                if (-not $text.Contains($marker)) { throw "$($fixture.Name) did not emit $marker" }
            }
            if ($text.Contains("EXPLORE:EXCEPTION")) { throw "$($fixture.Name) emitted an exception" }
            Write-Host "$($fixture.Name) UI Automation runtime passed"
        }
        finally {
            Remove-Item Env:REPROIT_TARGET -ErrorAction SilentlyContinue
            Remove-Item Env:REPROIT_FUZZ_BUDGET -ErrorAction SilentlyContinue
            Remove-Item Env:REPROIT_CONFIG -ErrorAction SilentlyContinue
            Remove-Item Env:REPROIT_DEVICE -ErrorAction SilentlyContinue
            Stop-Process -Name $fixture.Process -Force -ErrorAction SilentlyContinue
        }
    }
}
finally {
    Remove-Item Env:CARGO_TARGET_DIR -ErrorAction SilentlyContinue
    Pop-Location
}

Write-Host "Windows DesktopUia backend passed WPF, Avalonia, and WinUI"
