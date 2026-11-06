param([Parameter(Mandatory = $true)][string]$OutDir)

# Copies the vendored Mudfish WFP files next to the built executable and verifies each copy against its pinned hash.
# The application does not read this folder at runtime; it uses the bytes embedded in the executable.
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8
$sourceDir = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..\vendor\wfp')).Path
$outRoot = (New-Item -ItemType Directory -Force -Path $OutDir).FullName
$destDir = Join-Path $outRoot 'wfp'
$driversDir = Join-Path $outRoot 'DRIVERS'
New-Item -ItemType Directory -Force -Path $destDir, $driversDir | Out-Null

$pinned = [ordered]@{
    'MUDWFP.sys'        = 'F4E658F1F88FB6F3E1A0650BD0EE9F2FFEC3CBB2953064F0E7CE2226972BF3E9'
    'mudwfp_proxy.exe'  = '26BF54F45DCCAFA9625CB599A3460CE8B49E1D9A220614263B26FA4004398283'
    'mudwfp.inf'        = '659C950CD23595B5CE89621D35DEBBB86F709AAAAE053B40E6A75B3DBE38DEDB'
}
$stageTargets = @(
    @{ File = 'mudwfp_proxy.exe'; Rel = 'mudwfp_proxy.exe' },
    @{ File = 'MUDWFP.sys';       Rel = 'DRIVERS\MUDWFP.sys' },
    @{ File = 'MUDWFP.sys';       Rel = 'wfp\MUDWFP.sys' },
    @{ File = 'mudwfp_proxy.exe'; Rel = 'wfp\mudwfp_proxy.exe' },
    @{ File = 'mudwfp.inf';       Rel = 'wfp\mudwfp.inf' }
)
foreach ($entry in $stageTargets) {
    $name = $entry.File
    $rel = $entry.Rel
    $target = Join-Path $outRoot $rel
    Copy-Item -LiteralPath (Join-Path $sourceDir $name) -Destination $target -Force
    $actual = (Get-FileHash -LiteralPath $target -Algorithm SHA256).Hash
    if ($actual -ne $pinned[$name]) { throw "Hash divergente em ${rel}: $actual" }
    Write-Host "$rel ok ($actual)"
}
Copy-Item -LiteralPath (Join-Path $sourceDir 'NOTICE.md') -Destination (Join-Path $destDir 'NOTICE.md') -Force
$repoRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).Path
Copy-Item -LiteralPath (Join-Path $sourceDir 'NOTICE.md') -Destination (Join-Path $outRoot 'NOTICE.md') -Force
Copy-Item -LiteralPath (Join-Path $repoRoot 'native\hy2\THIRD_PARTY_NOTICES.txt') -Destination (Join-Path $outRoot 'THIRD_PARTY_NOTICES.txt') -Force
Copy-Item -LiteralPath (Join-Path $repoRoot 'ui-screen.svg') -Destination (Join-Path $outRoot 'ui-screen.svg') -Force
Copy-Item -LiteralPath (Join-Path $repoRoot 'ui-screen.json') -Destination (Join-Path $outRoot 'ui-screen.json') -Force
Copy-Item -LiteralPath (Join-Path $repoRoot 'ui-screen.png') -Destination (Join-Path $outRoot 'ui-screen.png') -Force

$exePath = Join-Path $outRoot 'rintel-rapo.exe'
if (Test-Path -LiteralPath $exePath) {
    $zipPath = Join-Path $outRoot 'rintel-rapo-windows-x64.zip'
    $zipShaPath = "$zipPath.sha256"
    if (Test-Path -LiteralPath $zipPath) { Remove-Item -LiteralPath $zipPath -Force }
    $itemsToZip = @(
        'rintel-rapo.exe',
        'rintel-rapo.exe.sha256',
        'rintel-rapo.cer',
        'mudwfp_proxy.exe',
        'DRIVERS',
        'wfp',
        'NOTICE.md',
        'THIRD_PARTY_NOTICES.txt',
        'ui-screen.svg',
        'ui-screen.json',
        'ui-screen.png'
    ) | ForEach-Object { Join-Path $outRoot $_ } | Where-Object { Test-Path -LiteralPath $_ }
    Compress-Archive -Path $itemsToZip -DestinationPath $zipPath -CompressionLevel Optimal -Force
    $zipHash = (Get-FileHash -LiteralPath $zipPath -Algorithm SHA256).Hash.ToLower()
    "$zipHash  rintel-rapo-windows-x64.zip" | Out-File -FilePath $zipShaPath -Encoding ascii
    Write-Host "rintel-rapo-windows-x64.zip ok ($zipHash)"
}
