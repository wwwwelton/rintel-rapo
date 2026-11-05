param(
    [string]$OutDir = "C:\dev\rapo-build\release-dist"
)

$ErrorActionPreference = "Stop"

Write-Host "=== Encerrando processos anteriores para liberar arquivos ==="
Get-Process -Name "rintel-rapo", "rintel-rapo-standalone" -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Milliseconds 500

Write-Host "=== Gerando Build Release Local para Producao ==="
Set-Location "C:\dev\rapo-src"
$env:CARGO_TARGET_DIR = "C:\dev\rapo-build\target"
$env:RAPO_HY2_LIBRARY = "C:\dev\rapo-build\rapo_hy2.dll"

# 1. Compilacao otimizada release nativa do binario principal
cargo build --release --locked --offline --bin rintel-rapo
$targetExe = "C:\dev\rapo-build\target\release\rintel-rapo.exe"
& "C:\dev\rapo-src\scripts\sign-release.ps1" -ExePath $targetExe

# 2. Executa o empacotamento completo do standalone (make-standalone ja executa o stage-full-release)
& "C:\dev\rapo-src\scripts\make-standalone.ps1" -StagedDir $OutDir -OutExe (Join-Path $OutDir "rintel-rapo-standalone.exe")
Write-Host "=== Build de Producao concluido com sucesso em: $OutDir ==="
Get-ChildItem $OutDir | Select-Object Name, Length, LastWriteTime
