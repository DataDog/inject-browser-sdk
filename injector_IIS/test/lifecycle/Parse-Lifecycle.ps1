[CmdletBinding()]
param([Parameter(Mandatory = $true)][string]$Path)

$tokens = $null
$errors = $null
[System.Management.Automation.Language.Parser]::ParseFile(
    $Path, [ref]$tokens, [ref]$errors) | Out-Null
if ($errors.Count -gt 0) {
    $errors | Format-List
    exit 1
}
Write-Output 'PARSE_OK'
