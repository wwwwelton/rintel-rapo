param(
    [string]$ExePath = (Join-Path $PSScriptRoot "..\target\release\rintel-rapo.exe")
)

$ErrorActionPreference = "Stop"

$exePath = (Resolve-Path -LiteralPath $ExePath -ErrorAction Stop).Path

$dir = Split-Path -Parent $exePath
$cerPath = Join-Path $dir "rintel-rapo.cer"
$shaPath = "$exePath.sha256"

Write-Host "Localizando ou criando certificado CodeSigning..."
$cert = Get-ChildItem Cert:\CurrentUser\My -CodeSigningCert | Where-Object { $_.Subject -eq "CN=Rintel RAPO Open Source, O=Rintel Project, C=BR" -and $_.HasPrivateKey -and $_.NotBefore -le (Get-Date) -and $_.NotAfter -gt (Get-Date).AddDays(1) } | Sort-Object NotAfter -Descending | Select-Object -First 1
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

Write-Host "Assinando $exePath com Authenticode SHA-256 e carimbo DigiCert..."
$sig = Set-AuthenticodeSignature `
  -FilePath $exePath `
  -Certificate $cert `
  -HashAlgorithm SHA256 `
  -TimestampServer "http://timestamp.digicert.com"

if (-not $sig.SignerCertificate) {
    Set-AuthenticodeSignature -FilePath $exePath -Certificate $cert -HashAlgorithm SHA256 | Out-Null
}
$sig = Get-AuthenticodeSignature -FilePath $exePath
if (-not $sig.SignerCertificate -or $sig.SignerCertificate.Thumbprint -ne $cert.Thumbprint -or $sig.Status -notin @("Valid", "NotTrusted")) {
    throw "Assinatura invalida: $($sig.Status) - $($sig.StatusMessage)"
}
if (-not $sig.TimeStamperCertificate) {
    Write-Warning "Sem carimbo de tempo: a assinatura depende da validade do certificado."
}

$hash = (Get-FileHash $exePath -Algorithm SHA256).Hash.ToLower()
"$hash  rintel-rapo.exe" | Out-File -FilePath $shaPath -Encoding ascii

Write-Host "Status da assinatura:"
Get-AuthenticodeSignature -FilePath $exePath | Select-Object Status, StatusMessage, SignerCertificate, TimeStamperCertificate | Format-List
