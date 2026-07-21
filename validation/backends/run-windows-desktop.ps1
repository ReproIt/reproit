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
    cargo build -p reproit --release
    if ($LASTEXITCODE -ne 0) { throw "reproit release build failed" }
    $runner = Join-Path $env:CARGO_TARGET_DIR "release/reproit.exe"
    if (-not (Test-Path $runner)) { throw "reproit release binary was not created" }

    $fixtures = @(
        @{
            Name = "WPF"
            Project = "examples/wpf-fixture/WpfFixture.csproj"
            Exe = "WpfFixture.exe"
            Process = "WpfFixture"
        },
        @{
            Name = "Avalonia"
            Project = "examples/avalonia-fixture/AvaloniaFixture.csproj"
            Exe = "AvaloniaFixture.exe"
            Process = "AvaloniaFixture"
        },
        @{
            Name = "WinUI"
            Project = "examples/winui-fixture/WinUiFixture.csproj"
            Exe = "WinUiFixture.exe"
            Process = "WinUiFixture"
        }
    )

    foreach ($fixture in $fixtures) {
        $publish = Join-Path $out $fixture.Name
        Remove-Item -Recurse -Force $publish -ErrorAction SilentlyContinue
        $args = @(
            "publish", $fixture.Project, "-c", "Release", "-r", "win-x64",
            "--self-contained", "false", "-o", $publish
        )
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
            $start = [System.Diagnostics.ProcessStartInfo]::new()
            $start.FileName = $runner
            $start.Arguments = "__uia"
            $start.UseShellExecute = $false
            $start.CreateNoWindow = $true
            $start.RedirectStandardOutput = $true
            $start.RedirectStandardError = $true
            $run = [System.Diagnostics.Process]::new()
            $run.StartInfo = $start
            if (-not $run.Start()) { throw "$($fixture.Name) UIA runner did not start" }
            $stdoutTask = $run.StandardOutput.ReadToEndAsync()
            $stderrTask = $run.StandardError.ReadToEndAsync()
            $finished = $run.WaitForExit(90000)
            if (-not $finished) {
                & taskkill.exe /PID $run.Id /T /F | Out-Null
                $run.WaitForExit()
                throw "$($fixture.Name) UIA runner timed out after 90 seconds"
            }
            $run.WaitForExit()
            $exitCode = $run.ExitCode
            # The target app may inherit the runner's redirected pipe handles.
            # Close it before awaiting EOF, otherwise successful runs can wait
            # forever for a descendant that the finally block has not reached.
            Stop-Process -Name $fixture.Process -Force -ErrorAction SilentlyContinue
            $stdoutText = $stdoutTask.GetAwaiter().GetResult()
            $stderrText = $stderrTask.GetAwaiter().GetResult()
            [System.IO.File]::WriteAllText($runOut, $stdoutText)
            [System.IO.File]::WriteAllText($runErr, $stderrText)
            [System.IO.File]::WriteAllText($log, $stdoutText + $stderrText)
            Write-Host $stdoutText
            if ($stderrText) { Write-Warning $stderrText }
            $text = Get-Content -Raw $log
            $markers = @(
                "EXPLORE:STATE", "EXPLORE:EDGE", "JOURNEY DONE", "All tests passed"
            )
            foreach ($marker in $markers) {
                if (-not $text.Contains($marker)) { throw "$($fixture.Name) did not emit $marker" }
            }
            if ($text.Contains("EXPLORE:EXCEPTION")) {
                throw "$($fixture.Name) emitted an exception"
            }
            if ($exitCode -ne 0) {
                throw "$($fixture.Name) UIA runner exited $exitCode"
            }
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
