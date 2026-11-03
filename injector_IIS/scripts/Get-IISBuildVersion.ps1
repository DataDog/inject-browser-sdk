function Get-IISBuildVersion {
    param(
        [string]$VersionFile = (Join-Path $PSScriptRoot '..\VERSION'),
        [string]$Major,
        [string]$Minor,
        [string]$Build
    )
    $raw = (Get-Content -Raw $VersionFile).Trim()
    if ($raw -notmatch '\A([0-9]+)\.([0-9]+)\.([0-9]+)(?:-(.+))?\z') {
        throw "Invalid IIS VERSION: '$raw' (expected MAJ.MIN.PATCH or MAJ.MIN.PATCH-PRE)"
    }
    $patch = $Matches[3]
    $version = @{ MAJ_VER = $Matches[1]; MIN_VER = $Matches[2]; VERSION_PRE = $Matches[4] }
    if (-not $version.VERSION_PRE) { $version.VERSION_PRE = 'dev' }
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
    # PATCH is not an MSI field; bound it like BUILD so all resolvers agree.
    $patch = $patch.TrimStart('0')
    if (-not $patch) { $patch = '0' }
    if ($patch.Length -gt 5 -or [long]$patch -gt 65535) { throw "PATCH=$patch exceeds 65535" }
    $version.RELEASE_VER = "$($version.MAJ_VER).$($version.MIN_VER).$patch"
    if ($env:IIS_RELEASE_BUILD -eq '1') { $version.VERSION_PRE = '' }
    return $version
}
