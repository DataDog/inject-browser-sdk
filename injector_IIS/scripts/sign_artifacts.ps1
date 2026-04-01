param(
    [Parameter(Mandatory)]
    [string]$ArtifactsDir
)

$ErrorActionPreference = "Stop"

$files = Get-ChildItem $ArtifactsDir -Include *.msi,*.dll,*.exe -Recurse
if ($files.Count -eq 0) {
    Write-Error "No signable artifacts found in $ArtifactsDir"
    exit 1
}

foreach ($f in $files) {
    Write-Host "Signing $($f.FullName)..."
    dd-wcs sign $f.FullName
    if ($LASTEXITCODE -ne 0) {
        Write-Error "Failed to sign $($f.FullName)"
        exit 1
    }
}
