# Fail unless every given file has a valid Authenticode signature.
param([Parameter(Mandatory=$true, ValueFromRemainingArguments=$true)][string[]]$Path)
$ErrorActionPreference = 'Stop'
foreach ($file in $Path) {
    $signature = Get-AuthenticodeSignature -FilePath $file
    if ($signature.Status -ne 'Valid') { throw "$file signature is $($signature.Status): $($signature.StatusMessage)" }
    Write-Host "$file is signed by $($signature.SignerCertificate.Subject)"
}
