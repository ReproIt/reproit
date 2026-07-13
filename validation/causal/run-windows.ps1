$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"
Set-StrictMode -Version Latest

$root = (Resolve-Path (Join-Path $PSScriptRoot "../..")).Path
$sdk = Join-Path $root "sdk/reproit-windows"
$arch = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString()
$os = [System.Runtime.InteropServices.RuntimeInformation]::OSDescription

if ([Environment]::OSVersion.Platform -ne [PlatformID]::Win32NT) {
    throw "The Windows-native causal gate must run on Windows; got $os"
}
if ($arch -notin @("X86", "X64")) {
    throw "Expected an x86-family Windows VM, got $arch"
}
if (-not (Get-Command dotnet -ErrorAction SilentlyContinue)) {
    throw ".NET SDK is not installed on the Windows VM"
}

Write-Host "Windows native gate: $os ($arch)"
dotnet --info
dotnet test (Join-Path $sdk "test/ReproIt.ParityTests/ReproIt.ParityTests.csproj") --configuration Release
dotnet build (Join-Path $sdk "src/ReproIt.Windows/ReproIt.Windows.csproj") --configuration Release

# CausalHandlerTest proves capture-time redaction, canonical URL matching,
# capsule fulfillment, CAPSULE:MISS, and that replay never calls the live
# handler. Re-run it alone so the hermetic contract is visible in gate output.
dotnet test (Join-Path $sdk "test/ReproIt.ParityTests/ReproIt.ParityTests.csproj") `
    --configuration Release --no-restore --filter "FullyQualifiedName~CausalHandlerTest"

Write-Host "native Windows causal capture/replay passed on $arch"
