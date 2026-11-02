:: Sign and repackage the release candidate in c:\mnt\dist into c:\mnt\signed.
:: Runs in the Windows build image from the sign_injector_IIS_release job.
if not exist c:\mnt\dist\injector_IIS.manifest goto nocandidate

:: Windows runners get re-used between jobs, so work on a copy of the checkout
mkdir \build
cd \build || exit /b 2
xcopy /e/s/h/q c:\mnt\*.* || exit /b 3

:: Install WiX 3 (the build image ships WiX 5, but our installer project uses v3)
powershell -Command "Invoke-WebRequest -Uri 'https://github.com/wixtoolset/wix3/releases/download/wix3141rtm/wix314.exe' -OutFile wix314.exe" || exit /b 5
start /wait wix314.exe /install /quiet /norestart || exit /b 5
set "PATH=%PATH%;C:\Program Files (x86)\WiX Toolset v3.14\bin"

pushd injector_iis
invoke iis.sign-candidate --candidate-dir \build\dist --out-dir \build\signed || exit /b 4
popd

if not exist c:\mnt\signed (mkdir c:\mnt\signed)
copy /y \build\signed\injector_IIS_installer.msi c:\mnt\signed\ || exit /b 6
copy /y \build\signed\injector_IIS.version c:\mnt\signed\ || exit /b 6
copy /y \build\signed\injector_IIS.release c:\mnt\signed\ || exit /b 6
goto :EOF

:nocandidate
@echo c:\mnt\dist has no release candidate
exit /b 1
