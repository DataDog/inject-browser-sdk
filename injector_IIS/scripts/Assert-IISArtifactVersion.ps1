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
function Get-MsiProperty([string]$Name) {
    # [void] keeps the COM calls' return values out of the function's output.
    $view = $database.OpenView("SELECT ``Value`` FROM ``Property`` WHERE ``Property`` = '$Name'")
    [void]$view.Execute()
    $record = $view.Fetch()
    [void]$view.Close()
    if (-not $record) { throw "MSI has no $Name" }
    return [string]$record.StringData(1)
}
$actual = Get-MsiProperty 'ProductVersion'
if ($actual -ne $expected) { throw "MSI ProductVersion $actual differs from expected $expected" }

# The display version carries the release (MAJ.MIN.PATCH) and, outside
# release builds, the prerelease suffix. Release builds must not be named -dev.
$release = if ($env:RELEASE_VER) { $env:RELEASE_VER } else { "$env:MAJ_VER.$env:MIN_VER.$env:BUILD_VER" }
$display = if ($env:VERSION_PRE) { "$release-$env:VERSION_PRE.$env:BUILD_VER" } else { $release }
$name = Get-MsiProperty 'ProductName'
if (-not $name.EndsWith(" $display")) { throw "MSI ProductName '$name' does not end with version $display" }
if ($env:IIS_RELEASE_BUILD -eq '1' -and $display.Contains('-')) {
    throw "Release build has prerelease display version $display"
}
Write-Host "Verified matching DLL and MSI versions: $expected ($name)"
