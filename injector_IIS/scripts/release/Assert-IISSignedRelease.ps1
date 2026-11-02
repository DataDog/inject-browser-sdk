# Check a repackaged release MSI against the tested candidate it came from:
# valid Authenticode signatures (when signing), the signed DLL inside it, and
# the same installed files as the candidate.
param(
    [Parameter(Mandatory=$true)][string]$MsiPath,
    [Parameter(Mandatory=$true)][string]$DllPath,
    [Parameter(Mandatory=$true)][string]$CandidateMsiPath,
    [switch]$RequireSignature
)
$ErrorActionPreference = 'Stop'

if ($RequireSignature) {
    foreach ($path in @($DllPath, $MsiPath)) {
        $signature = Get-AuthenticodeSignature -FilePath $path
        if ($signature.Status -ne 'Valid') { throw "$path signature is $($signature.Status): $($signature.StatusMessage)" }
        Write-Host "$path is signed by $($signature.SignerCertificate.Subject)"
    }
} else {
    Write-Host "Signing disabled (rehearsal): checking contents only"
}

# An administrative install extracts the MSI payload without installing it.
function Expand-Msi([string]$Path) {
    $target = Join-Path ([IO.Path]::GetTempPath()) ([IO.Path]::GetRandomFileName())
    $process = Start-Process msiexec.exe -ArgumentList @('/a', "`"$((Resolve-Path $Path).Path)`"", '/qn', "TARGETDIR=`"$target`"") -Wait -PassThru
    if ($process.ExitCode -ne 0) { throw "Extracting $Path failed: $($process.ExitCode)" }
    return $target
}
function Get-Payload([string]$Root) {
    Get-ChildItem $Root -Recurse -File | Where-Object { $_.Extension -ne '.msi' } |
        ForEach-Object { $_.FullName.Substring($Root.Length) } | Sort-Object
}

$signed = Expand-Msi $MsiPath
$candidate = Expand-Msi $CandidateMsiPath
$dlls = @(Get-ChildItem $signed -Recurse -Filter 'injector_IIS.dll')
if ($dlls.Count -ne 1) { throw "Expected one injector_IIS.dll in $MsiPath, found $($dlls.Count)" }
$expected = (Get-FileHash $DllPath -Algorithm SHA256).Hash
if ((Get-FileHash $dlls[0].FullName -Algorithm SHA256).Hash -ne $expected) { throw "$MsiPath does not contain $DllPath" }
$difference = Compare-Object @(Get-Payload $candidate) @(Get-Payload $signed)
if ($difference) { throw "Installed files differ from the candidate: $($difference | Out-String)" }
Write-Host "Verified $MsiPath against $CandidateMsiPath"
