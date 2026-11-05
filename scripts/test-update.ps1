param(
    [string]$StandaloneExe = "C:\dev\rapo-build\target\release\rintel-rapo-standalone.exe"
)

$ErrorActionPreference = "Stop"

if (-not (Test-Path -LiteralPath $StandaloneExe)) {
    throw "Executavel standalone nao encontrado em $StandaloneExe"
}

Write-Host "Iniciando teste simulado do standalone com mock de versao e rede..."

# Criamos uma cópia de teste para não interferir na build oficial
$testExe = Join-Path (Split-Path $StandaloneExe) "rintel-rapo-test-update.exe"
Copy-Item -LiteralPath $StandaloneExe -Destination $testExe -Force

Write-Host "Copia de teste criada em: $testExe"
Write-Host "Para testar o fluxo de update:"
Write-Host "1. A versão atual hardcoded no launcher é 'v0.2.1'."
Write-Host "2. Quando o executável for aberto, ele consultará a API do GitHub para 'releases/latest'."
Write-Host "3. Se a tag remota for diferente de 'v0.2.1' (ex: v0.2.5), ele exibirá o prompt de atualização."
Write-Host "4. Se você clicar em 'Sim', ele fará o download do novo standalone, substituirá e relançará."
Write-Host "Executando teste interativo..."

& $testExe
