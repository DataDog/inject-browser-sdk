function Get-IISBuildVersion {
    param(
        [string]$VersionFile = (Join-Path $PSScriptRoot '..\VERSION'),
        [string]$Major,
        [string]$Minor,
        [string]$Build
    )
    $raw = (Get-Content -Raw $VersionFile).Trim()
    if ($raw -notmatch '\A([0-9]+)\.([0-9]+)(?:-(.+))?\z') {
        throw "Invalid IIS VERSION: '$raw' (expected MAJ.MIN or MAJ.MIN-PRE)"
    }
    $version = @{ MAJ_VER = $Matches[1]; MIN_VER = $Matches[2]; VERSION_PRE = $Matches[3] }
    if (-not $version.VERSION_PRE) { $version.VERSION_PRE = '' }
    if ($PSBoundParameters.ContainsKey('Major')) { $version.MAJ_VER = $Major }
    if ($PSBoundParameters.ContainsKey('Minor')) { $version.MIN_VER = $Minor }
    if (-not $PSBoundParameters.ContainsKey('Build')) {
        $Build = $env:CI_PIPELINE_IID
        if (-not $Build) { $Build = '0' }
    }
    $version.BUILD_VER = $Build
    foreach ($field in @('MAJ_VER', 'MIN_VER', 'BUILD_VER')) {
        $value = $version[$field]
        if ($value -cnotmatch '\A[0-9]+\z') { throw "$field must contain decimal digits, got '$value'" }
        $value = $value.TrimStart('0')
        if (-not $value) { $value = '0' }
        $maximum = if ($field -eq 'BUILD_VER') { 65535 } else { 255 }
        if ($value.Length -gt "$maximum".Length -or [long]$value -gt $maximum) {
            throw "$field=$value exceeds the Windows Installer limit ($maximum)"
        }
        $version[$field] = $value
    }
    if ($env:IIS_RELEASE_BUILD -eq '1') { $version.VERSION_PRE = '' }
    return $version
}
