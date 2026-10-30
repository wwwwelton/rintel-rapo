param(
    [string]$Target = "x86_64-pc-windows-gnu"
)

$ErrorActionPreference = "Stop"

$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$repoDir = Split-Path -Parent $scriptDir

$exePaths = @(
    (Join-Path $repoDir "target\$Target\release\rintel-rapo.exe"),
    (Join-Path $repoDir "target\release\rintel-rapo.exe")
)
$exePath = $exePaths | Where-Object { Test-Path $_ } | Select-Object -First 1

if (-not $exePath) {
    Write-Error "Executavel de release nao encontrado. Compile primeiro com: cargo build --release"
    exit 1
}

$dir = Split-Path -Parent $exePath
$cerPath = Join-Path $dir "rintel-rapo.cer"
$shaPath = "$exePath.sha256"

Write-Host "Localizando ou criando certificado CodeSigning..."
$cert = Get-ChildItem Cert:\CurrentUser\My -CodeSigningCert | Where-Object { $_.Subject -like "*Rintel RAPO*" } | Select-Object -First 1
if (-not $cert) {
    $cert = New-SelfSignedCertificate `
      -Type CodeSigningCert `
      -Subject "CN=Rintel RAPO Open Source, O=Rintel Project, C=BR" `
      -KeyAlgorithm RSA `
      -KeyLength 2048 `
      -HashAlgorithm SHA256 `
      -CertStoreLocation "Cert:\CurrentUser\My" `
      -NotAfter (Get-Date).AddYears(5)
}

Export-Certificate -Cert $cert -FilePath $cerPath -Force | Out-Null
try { Import-Certificate -FilePath $cerPath -CertStoreLocation "Cert:\CurrentUser\Root" -ErrorAction SilentlyContinue | Out-Null } catch {}

Write-Host "Assinando $exePath com Authenticode SHA-256 e carimbo DigiCert..."
$sig = Set-AuthenticodeSignature `
  -FilePath $exePath `
  -Certificate $cert `
  -HashAlgorithm SHA256 `
  -TimestampServer "http://timestamp.digicert.com"

if (-not $sig.SignerCertificate) {
    Set-AuthenticodeSignature -FilePath $exePath -Certificate $cert -HashAlgorithm SHA256 | Out-Null
}

$hash = (Get-FileHash $exePath -Algorithm SHA256).Hash.ToLower()
"$hash  rintel-rapo.exe" | Out-File -FilePath $shaPath -Encoding ascii

Write-Host "Status da assinatura:"
Get-AuthenticodeSignature -FilePath $exePath | Select-Object Status, StatusMessage, SignerCertificate, TimeStamperCertificate | Format-List
