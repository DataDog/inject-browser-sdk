if not exist c:\mnt\ goto nomntdir

:: Windows runners get re-used between jobs, so we must make sure to run our builds in a temporary location
mkdir \build
cd \build || exit /b 2
xcopy /e/s/h/q c:\mnt\*.* || exit /b 3

set "outdir=%cd%\build_out"
if exist %outdir% (rmdir /q/s %outdir%)

:: Install WiX 3 (the build image ships WiX 5, but our installer project uses v3)
powershell -Command "Invoke-WebRequest -Uri 'https://github.com/wixtoolset/wix3/releases/download/wix3141rtm/wix314.exe' -OutFile wix314.exe" || exit /b 5
start /wait wix314.exe /install /quiet /norestart || exit /b 5
set "PATH=%PATH%;C:\Program Files (x86)\WiX Toolset v3.14\bin"

python -m unittest discover -s injector_IIS/test/version_tests || exit /b 8

pushd injector_iis
invoke iis.build --out-dir %outdir% || exit /b 4
popd

dir %outdir%

if not exist %BUILD_OUTDIR% (mkdir %BUILD_OUTDIR%)
copy /y %outdir%\*.msi %BUILD_OUTDIR% || exit /b 6
copy /y %outdir%\*.dll %BUILD_OUTDIR% || exit /b 7
copy /y %outdir%\injector_IIS.version %BUILD_OUTDIR% || exit /b 8
copy /y %outdir%\injector_IIS.release %BUILD_OUTDIR% || exit /b 8
:: Files for the OCI package (.gitlab/prepare-oci-package.sh).
for %%f in (RUMTools.ps1 Datadog.RUM.psm1 Datadog.RUM.psd1 RUM_schema.xml) do (
    copy /y %outdir%\%%f %BUILD_OUTDIR% || exit /b 9
)

goto :EOF

:nomntdir
@echo directory not mounted, parameters incorrect
exit /b 1
goto :EOF
