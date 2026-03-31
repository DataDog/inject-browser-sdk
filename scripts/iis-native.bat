@echo off
:: iis-native.bat <configure|build|test|shell>
::
:: Sets up the MSVC + Rust environment and runs IIS injector build tasks natively.
:: Designed to be called from Git Bash via: cmd.exe /c scripts\iis-native.bat <target>
::
:: Usage:
::   cmd.exe /c scripts\iis-native.bat configure
::   cmd.exe /c scripts\iis-native.bat build
::   cmd.exe /c scripts\iis-native.bat test

setlocal EnableDelayedExpansion

set TARGET=%~1
if "%TARGET%"=="" set TARGET=build

:: --- Locate Visual Studio via vswhere ---
set VSWHERE="%ProgramFiles(x86)%\Microsoft Visual Studio\Installer\vswhere.exe"
if not exist %VSWHERE% (
    echo ERROR: vswhere.exe not found. Install Visual Studio Build Tools.
    exit /b 1
)

for /f "usebackq tokens=*" %%i in (`%VSWHERE% -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath`) do (
    set VS_PATH=%%i
)

if "!VS_PATH!"=="" (
    echo ERROR: No VS installation with VC tools found.
    exit /b 1
)

set VSDEVCMD=!VS_PATH!\Common7\Tools\VsDevCmd.bat
if not exist "!VSDEVCMD!" (
    echo ERROR: VsDevCmd.bat not found at !VSDEVCMD!
    exit /b 1
)

:: --- Activate VS dev environment ---
call "!VSDEVCMD!" -arch=x64 -host_arch=x64
if errorlevel 1 (
    echo ERROR: Failed to activate VS dev environment.
    exit /b 1
)

:: --- Add Rust/cargo to PATH ---
set PATH=%PATH%;%USERPROFILE%\.cargo\bin

:: --- Verify tools ---
where cl >nul 2>&1 || (echo ERROR: cl.exe not found after VS env setup. & exit /b 1)
where cargo >nul 2>&1 || (echo ERROR: cargo not found. Install Rust via: winget install Rustlang.Rustup & exit /b 1)

:: --- Set build directory relative to script location ---
set REPO_DIR=%~dp0..
set BUILD_DIR=%REPO_DIR%\injector_IIS\build
set SOURCE_DIR=%REPO_DIR%\injector_IIS

echo.
echo VS:    !VS_PATH!
echo Repo:  %REPO_DIR%
echo Build: %BUILD_DIR%
echo.

if /i "%TARGET%"=="configure" goto do_configure
if /i "%TARGET%"=="build"     goto do_build
if /i "%TARGET%"=="test"      goto do_test
if /i "%TARGET%"=="shell"     goto do_shell

echo ERROR: Unknown target "%TARGET%". Use: configure, build, test, shell
exit /b 1

:do_configure
echo === Configuring ===
cmake --preset=ci-native -B "%BUILD_DIR%" -DCMAKE_BUILD_TYPE=Debug "%SOURCE_DIR%"
exit /b %ERRORLEVEL%

:do_build
echo === Configuring + Building ===
cmake --preset=ci-native -B "%BUILD_DIR%" -DCMAKE_BUILD_TYPE=Debug "%SOURCE_DIR%"
if errorlevel 1 exit /b %ERRORLEVEL%
cmake --build "%BUILD_DIR%" -j
exit /b %ERRORLEVEL%

:do_test
echo === Running unit tests ===
"%BUILD_DIR%\test\unittests\iis_injector_tests.exe"
exit /b %ERRORLEVEL%

:do_shell
echo === Opening VS Developer shell ===
cmd.exe /k
exit /b 0
