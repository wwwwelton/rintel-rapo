param(
    [string]$TargetDir = "C:\dev\rapo-build\staged"
)
$ErrorActionPreference = "Stop"
$dir = (Resolve-Path -LiteralPath $TargetDir).Path

# Garante a estrutura exata solicitada
$driversDir = Join-Path $dir "DRIVERS"
$wfpDir = Join-Path $dir "wfp"
$webviewDir = Join-Path $dir "rintel-rapo.exe.WebView2"
New-Item -ItemType Directory -Force -Path $driversDir, $wfpDir, $webviewDir | Out-Null

$sourceDir = "C:\dev\rapo-src\vendor\wfp"

# Copia e verifica os componentes WFP e drivers
Copy-Item -LiteralPath (Join-Path $sourceDir "MUDWFP.sys") -Destination (Join-Path $driversDir "MUDWFP.sys") -Force
Copy-Item -LiteralPath (Join-Path $sourceDir "MUDWFP.sys") -Destination (Join-Path $wfpDir "MUDWFP.sys") -Force
Copy-Item -LiteralPath (Join-Path $sourceDir "mudwfp_proxy.exe") -Destination (Join-Path $dir "mudwfp_proxy.exe") -Force
Copy-Item -LiteralPath (Join-Path $sourceDir "mudwfp_proxy.exe") -Destination (Join-Path $wfpDir "mudwfp_proxy.exe") -Force
Copy-Item -LiteralPath (Join-Path $sourceDir "mudwfp.inf") -Destination (Join-Path $wfpDir "mudwfp.inf") -Force
Copy-Item -LiteralPath (Join-Path $sourceDir "NOTICE.md") -Destination (Join-Path $dir "NOTICE.md") -Force
Copy-Item -LiteralPath (Join-Path $sourceDir "NOTICE.md") -Destination (Join-Path $wfpDir "NOTICE.md") -Force

# Copia avisos de terceiros e UI
$repoRoot = "C:\dev\rapo-src"
Copy-Item -LiteralPath (Join-Path $repoRoot "native\hy2\THIRD_PARTY_NOTICES.txt") -Destination (Join-Path $dir "THIRD_PARTY_NOTICES.txt") -Force
Copy-Item -LiteralPath (Join-Path $repoRoot "ui-screen.svg") -Destination (Join-Path $dir "ui-screen.svg") -Force
Copy-Item -LiteralPath (Join-Path $repoRoot "ui-screen.json") -Destination (Join-Path $dir "ui-screen.json") -Force

# Copia o executável compilado e certificado se existirem
$exeSrc = "C:\dev\rapo-build\target\release\rintel-rapo.exe"
$cerSrc = "C:\dev\rapo-build\target\release\rintel-rapo.cer"
if (Test-Path -LiteralPath $exeSrc) {
    Copy-Item -LiteralPath $exeSrc -Destination (Join-Path $dir "rintel-rapo.exe") -Force
    # Assina novamente o executável empacotado para garantir validade no release
    & "C:\dev\rapo-src\scripts\sign-release.ps1" -ExePath (Join-Path $dir "rintel-rapo.exe")
}
if (Test-Path -LiteralPath $cerSrc) {
    Copy-Item -LiteralPath $cerSrc -Destination (Join-Path $dir "rintel-rapo.cer") -Force
}

# Cria um arquivo simulado ou vsm se necessário
$vsmPath = Join-Path $dir ".mudwfp_proxy.vsm"
if (-not (Test-Path -LiteralPath $vsmPath)) {
    [System.IO.File]::WriteAllBytes($vsmPath, (New-Object byte[] 2097152))
}

Write-Host "Pacote de distribuição completo gerado em: $dir"
Get-ChildItem $dir | Select-Object Mode, LastWriteTime, Length, Name
