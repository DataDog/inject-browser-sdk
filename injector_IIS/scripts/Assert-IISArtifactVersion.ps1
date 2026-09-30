param(
    [Parameter(Mandatory=$true)][string]$DllPath,
    [Parameter(Mandatory=$true)][string]$MsiPath
)
$ErrorActionPreference = 'Stop'
$expected = "$env:MAJ_VER.$env:MIN_VER.$env:BUILD_VER.0"
$dll = [Diagnostics.FileVersionInfo]::GetVersionInfo((Resolve-Path $DllPath).Path)
$actual = "$($dll.FileMajorPart).$($dll.FileMinorPart).$($dll.FileBuildPart).$($dll.FilePrivatePart)"
if ($actual -ne $expected) { throw "DLL version $actual differs from expected $expected" }
$installer = New-Object -ComObject WindowsInstaller.Installer
$database = $installer.OpenDatabase((Resolve-Path $MsiPath).Path, 0)
$view = $database.OpenView("SELECT ``Value`` FROM ``Property`` WHERE ``Property`` = 'ProductVersion'")
$view.Execute()
$record = $view.Fetch()
if (-not $record) { throw 'MSI has no ProductVersion' }
$actual = $record.StringData(1)
$view.Close()
if ($actual -ne $expected) { throw "MSI ProductVersion $actual differs from expected $expected" }
Write-Host "Verified matching DLL and MSI versions: $expected"
