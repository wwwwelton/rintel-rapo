param(
    [string]$OutDir = "C:\dev\rapo-build\release-dist"
)

$ErrorActionPreference = "Stop"

Write-Host "=== Gerando Build Release Local para Usuário Final (v0.2.7) ==="

$env:RAPO_HY2_LIBRARY = "C:\dev\rapo-build\rapo_hy2.dll"

# 1. Compilação otimizada release nativa do binário principal
cargo build --release --locked --offline
if ($LASTEXITCODE -ne 0) { throw "Falha na compilacao release do Rust" }

# 2. Executa o empacotamento completo de staging e standalone
& "C:\dev\rapo-src\scripts\stage-full-release.ps1" -TargetDir $OutDir
& "C:\dev\rapo-src\scripts\make-standalone.ps1" -StagedDir $OutDir -OutExe (Join-Path $OutDir "rintel-rapo-standalone.exe")

Write-Host "=== Build de Usuário Final concluído com sucesso em: $OutDir ==="
Get-ChildItem $OutDir | Select-Object Name, Length, LastWriteTime
