param([Parameter(Mandatory = $true)][string]$OutDir)

# Copies the vendored Mudfish WFP files next to the built executable and verifies each copy against its pinned hash.
# The application does not read this folder at runtime; it uses the bytes embedded in the executable.
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8
$sourceDir = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..\vendor\wfp')).Path
$destDir = Join-Path (New-Item -ItemType Directory -Force -Path $OutDir).FullName 'wfp'
New-Item -ItemType Directory -Force -Path $destDir | Out-Null

$pinned = [ordered]@{
    'MUDWFP.sys'        = 'F4E658F1F88FB6F3E1A0650BD0EE9F2FFEC3CBB2953064F0E7CE2226972BF3E9'
    'mudwfp_proxy.exe'  = '26BF54F45DCCAFA9625CB599A3460CE8B49E1D9A220614263B26FA4004398283'
    'mudwfp.inf'        = '659C950CD23595B5CE89621D35DEBBB86F709AAAAE053B40E6A75B3DBE38DEDB'
}
foreach ($name in $pinned.Keys) {
    $target = Join-Path $destDir $name
    Copy-Item -LiteralPath (Join-Path $sourceDir $name) -Destination $target -Force
    $actual = (Get-FileHash -LiteralPath $target -Algorithm SHA256).Hash
    if ($actual -ne $pinned[$name]) { throw "Hash divergente em ${name}: $actual" }
    Write-Host "wfp\$name ok ($actual)"
}
Copy-Item -LiteralPath (Join-Path $sourceDir 'NOTICE.md') -Destination (Join-Path $destDir 'NOTICE.md') -Force
