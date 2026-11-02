param(
    [string]$Preset = "ci-native",
    [string]$BuildDir = "build",
    [string]$BuildType = "Release"
)

$ErrorActionPreference = "Stop"

Set-Location $PSScriptRoot\..\..

# Initialize MSVC environment via VsDevCmd.bat (same approach as the build task).
# The Windows build image sets VSINSTALLDIR; fall back to VSTUDIO_ROOT.
$vsRoot = $env:VSINSTALLDIR
if (-not $vsRoot) { $vsRoot = $env:VSTUDIO_ROOT }
if (-not $vsRoot) {
    Write-Error "VSINSTALLDIR / VSTUDIO_ROOT not set - cannot find Visual Studio"
    exit 1
}
$vsDevCmd = Join-Path $vsRoot "Common7\Tools\VsDevCmd.bat"

# The build image installs the C++ workload without recommended components,
# which excludes the AddressSanitizer runtime. Install the optional component
# only for the sanitizer job, using the same pinned bootstrapper as the image.
if ($Preset -eq "ci-native-asan") {
    $asanLibrary = Get-ChildItem (Join-Path $vsRoot "VC\Tools\MSVC") -Recurse `
        -Filter "clang_rt.asan_static_runtime_thunk-x86_64.lib" -ErrorAction SilentlyContinue
    if (-not $asanLibrary) {
        # Match windows_ltsc2022_x64:v113129110-33018b2a in .gitlab/iis.yml.
        # SoftwareTable variables from the image build are not persisted at runtime.
        $bootstrapperUrl = "https://download.visualstudio.microsoft.com/download/pr/3014ff35-1082-4da0-ba77-9d3664b7c38a/52367ab60e0f223d3f1d9c3406130e07ab711d7d48f922a407ccdb45b62937dd/vs_BuildTools.exe"
        $bootstrapperHash = "52367AB60E0F223D3F1D9C3406130E07AB711D7D48F922A407CCDB45B62937DD"
        $bootstrapper = Join-Path ([IO.Path]::GetTempPath()) "iis-asan-vs-buildtools.exe"
        try {
            Invoke-WebRequest -UseBasicParsing -Uri $bootstrapperUrl -OutFile $bootstrapper
            if ((Get-FileHash $bootstrapper -Algorithm SHA256).Hash -ne $bootstrapperHash) {
                throw "Visual Studio bootstrapper checksum mismatch"
            }
            $arguments = 'modify --installPath "' + $vsRoot + '" --add Microsoft.VisualStudio.Component.VC.ASAN --quiet --norestart --wait --nocache'
            $install = Start-Process -FilePath $bootstrapper -ArgumentList $arguments -Wait -PassThru
            if ($install.ExitCode -notin @(0, 3010)) {
                throw "AddressSanitizer component installation failed: $($install.ExitCode)"
            }
        } finally {
            Remove-Item $bootstrapper -Force -ErrorAction SilentlyContinue
        }
    }
}

# Run cmake inside a cmd session that sources VsDevCmd.bat first
# (matches the approach used by the invoke build task in iis.py).
# Use single-quoted here-string to avoid PowerShell 5.1 parsing && as pipeline chain.
$configureCmd = 'call "' + $vsDevCmd + '" -arch=amd64 && cmake --preset=' + $Preset + ' -B ' + $BuildDir + ' -DCMAKE_BUILD_TYPE=' + $BuildType
cmd /c $configureCmd
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

$buildCmd = 'call "' + $vsDevCmd + '" -arch=amd64 && cmake --build ' + $BuildDir + ' -j'
cmd /c $buildCmd
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

$testExe = Join-Path $BuildDir "test\unittests\iis_injector_tests.exe"
# Run in the same developer environment so the matching ASAN runtime DLL is
# on PATH when an instrumented binary starts.
$testCmd = 'call "' + $vsDevCmd + '" -arch=amd64 && "' + $testExe + '"'
cmd /c $testCmd
exit $LASTEXITCODE
