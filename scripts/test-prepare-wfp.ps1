$ErrorActionPreference = 'Stop'
$temp = Join-Path ([IO.Path]::GetTempPath()) ('rapo-wfp-reject-' + [Guid]::NewGuid().ToString('N'))
[IO.Directory]::CreateDirectory($temp) | Out-Null
try {
    $bytes = [IO.File]::ReadAllBytes((Join-Path $PSScriptRoot '..\vendor\wfp\MUDWFP.sys'))
    $bytes[0] = $bytes[0] -bxor 255
    $bad = Join-Path $temp 'MUDWFP.sys'
    [IO.File]::WriteAllBytes($bad, $bytes)
    $installed = Join-Path ([Environment]::SystemDirectory) 'drivers\MUDWFP.sys'
    $before = if (Test-Path -LiteralPath $installed) { (Get-FileHash -LiteralPath $installed -Algorithm SHA256).Hash } else { $null }
    $rejected = $false
    try { & (Join-Path $PSScriptRoot 'prepare-wfp.ps1') -SourcePath $bad } catch {
        if ($_.Exception.Message -notlike '*nao corresponde ao bundle verificado*') { throw }
        $rejected = $true
    }
    if (-not $rejected) { throw 'Corrupted same-size driver was not rejected.' }
    $after = if (Test-Path -LiteralPath $installed) { (Get-FileHash -LiteralPath $installed -Algorithm SHA256).Hash } else { $null }
    if ($before -ne $after) { throw 'Rejecting a corrupt driver changed the installed driver.' }
    Write-Output 'PASS: corrupted same-size driver rejected before installation.'
} finally { Remove-Item -LiteralPath $temp -Recurse -Force }
