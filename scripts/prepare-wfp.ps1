param([string]$SourcePath = $env:RAPO_WFP_DRIVER_SOURCE)

$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8
$expectedHash = 'F4E658F1F88FB6F3E1A0650BD0EE9F2FFEC3CBB2953064F0E7CE2226972BF3E9'
$source = (Resolve-Path -LiteralPath $SourcePath).Path
$bytes = [System.IO.File]::ReadAllBytes($source)
$sha = [System.Security.Cryptography.SHA256]::Create()
try {
    $hash = [BitConverter]::ToString($sha.ComputeHash($bytes)).Replace('-', '')
} finally { $sha.Dispose() }
if ($hash -ne $expectedHash) { throw 'O driver extraido nao corresponde ao bundle verificado.' }
$signature = Get-AuthenticodeSignature -LiteralPath $source
if ($signature.Status -ne 'Valid') { throw "Assinatura do driver recusada: $($signature.Status)." }

$destination = Join-Path ([Environment]::SystemDirectory) 'drivers\MUDWFP.sys'
$serviceKey = 'HKLM:\SYSTEM\CurrentControlSet\Services\MUDWFP'
if (Test-Path -LiteralPath $serviceKey) {
    $config = Get-ItemProperty -LiteralPath $serviceKey
    $image = [Environment]::ExpandEnvironmentVariables($config.ImagePath).Trim('"')
    if ($image.StartsWith('\??\')) { $image = $image.Substring(4) }
    if ($image.StartsWith('\SystemRoot\', [StringComparison]::OrdinalIgnoreCase)) {
        $image = Join-Path (Split-Path ([Environment]::SystemDirectory)) $image.Substring('\SystemRoot\'.Length)
    }
    if ($image -ine $destination -or $config.Type -ne 1) {
        throw 'Servico MUDWFP existente tem configuracao diferente; nada sera sobrescrito.'
    }
}
if (Test-Path -LiteralPath $destination) {
    if ((Get-FileHash -LiteralPath $destination -Algorithm SHA256).Hash -ne $expectedHash) {
        throw 'Driver MUDWFP instalado difere do bundle; atualize pelo fornecedor, sem sobrescrever terceiros.'
    }
} else {
    # Write the already verified bytes, not a second read from the user-writable extraction directory.
    $file = [System.IO.File]::Open($destination, [System.IO.FileMode]::CreateNew, [System.IO.FileAccess]::Write, [System.IO.FileShare]::None)
    $written = $false
    try {
        $file.Write($bytes, 0, $bytes.Length)
        $file.Flush($true)
        $written = $true
    } finally {
        $file.Dispose()
        if (-not $written) { [System.IO.File]::Delete($destination) }
    }
}
if ((Get-AuthenticodeSignature -LiteralPath $destination).Status -ne 'Valid') {
    throw 'A assinatura do driver instalado nao e valida; o servico nao sera iniciado.'
}
if (-not (Test-Path -LiteralPath $serviceKey)) {
    & (Join-Path ([Environment]::SystemDirectory) 'sc.exe') create MUDWFP type= kernel start= demand binPath= $destination
    if ($LASTEXITCODE -ne 0) { throw "Criacao do servico MUDWFP falhou: $LASTEXITCODE." }
}
$service = Get-Service -Name MUDWFP
if ($service.Status -ne [System.ServiceProcess.ServiceControllerStatus]::Running) {
    Start-Service -Name MUDWFP
    $service.Refresh()
}
$service.WaitForStatus([System.ServiceProcess.ServiceControllerStatus]::Running, [TimeSpan]::FromSeconds(8))
Write-Output 'MUDWFP_RUNNING'
