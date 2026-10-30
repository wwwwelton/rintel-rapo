$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
[Console]::OutputEncoding = New-Object System.Text.UTF8Encoding($false)
function Field($p, $names) {
    foreach ($name in $names) {
        $value = $p.CimInstanceProperties | Where-Object { $_.Name -eq $name } | Select-Object -First 1
        if ($null -ne $value) { return $value.Value }
        $value = $p.PSObject.Properties[$name]
        if ($null -ne $value) { return $value.Value }
    }
    return $null
}
function Matches($p, $intent) {
    $app = [string](Field $p @('AppPathNameMatchCondition', 'AppPathName'))
    $dscp = Field $p @('DSCPAction', 'DSCPValue')
    $rate = Field $p @('ThrottleRateActionBitsPerSecond', 'ThrottleRateAction', 'ThrottleRate')
    $precedence = Field $p @('Precedence')
    if ($app -ine [string]$intent.exe -or $null -eq $dscp -or [int]$dscp -ne [int]$intent.dscp -or $null -eq $rate -or [string]$rate -notin @('0','-1','18446744073709551615') -or $null -eq $precedence -or [int]$precedence -ne 254) { return $false }
    if ([string](Field $p @('NetworkProfile')) -notin @('All','7')) { return $false }
    foreach ($property in $p.CimInstanceProperties) {
        if ($property.Name -match '(Condition|Match)$' -and $property.Name -notmatch '^AppPathName' -and [string]$property.Value -notin @('', '0', '-1', '256', '65535', '4294967295', 'Any', 'Both', 'False')) { return $false }
        if ($property.Name -match 'Action$' -and $property.Name -notmatch '^(DSCP|ThrottleRate)' -and [string]$property.Value -notin @('', '0', '-1', '4294967295')) { return $false }
    }
    return $true
}
$owned = @()
$errorText = ''
try {
    $request = [Console]::In.ReadToEnd() | ConvertFrom-Json
    $owned = @($request.owned)
    foreach ($intent in $owned) { $intent.confirmed = $false }
    $all = @(Get-NetQosPolicy -PolicyStore ActiveStore -ErrorAction Stop)
    # Remove obsolete priorities first so switching a game never leaves a competing background rule.
    foreach ($intent in $owned) {
        $keep = @($request.desired | Where-Object { [string]$_[0] -ieq [string]$intent.exe -and [int]$_[1] -eq [int]$intent.dscp }).Count -gt 0
        if ($keep) { continue }
        $existing = @($all | Where-Object { $_.Name -ieq $intent.name })
        if ($existing.Count -gt 1 -or ($existing.Count -eq 1 -and -not (Matches $existing[0] $intent))) { throw ('Política DSCP alterada externamente; preservada: ' + $intent.name) }
        if ($existing.Count -eq 1) { Remove-NetQosPolicy -Name $intent.name -PolicyStore ActiveStore -Confirm:$false -ErrorAction Stop }
        $owned = @($owned | Where-Object { $_.name -ine $intent.name })
    }
    foreach ($intent in $owned) {
        if ($intent.name -inotmatch '^RintelRAPO-DSCP-[0-9]+-[0-9]+-[0-9]+$' -or [int]$intent.dscp -notin @(8,46) -or -not [IO.Path]::IsPathRooted([string]$intent.exe) -or [IO.Path]::GetExtension([string]$intent.exe) -ine '.exe' -or -not [IO.File]::Exists([string]$intent.exe)) { throw 'Identidade DSCP ou executável inválido' }
        $all = @(Get-NetQosPolicy -PolicyStore ActiveStore -ErrorAction Stop)
        $existing = @($all | Where-Object { $_.Name -ieq $intent.name })
        if ($existing.Count -eq 0) {
            foreach ($foreign in $all) {
                if ($foreign.Name -imatch '^rintelrapo-dscp-') { continue }
                $app = [string](Field $foreign @('AppPathNameMatchCondition','AppPathName'))
                if ($app -eq '' -or $app -eq '*') { throw ('Política QoS global/wildcard é ambígua; preservada: ' + $foreign.Name) }
                if ([string]$intent.exe -ieq $app -or [IO.Path]::GetFileName([string]$intent.exe) -ieq $app) { throw ('Política QoS anterior sobrepõe o executável; preservada: ' + $foreign.Name) }
            }
            $null = New-NetQosPolicy -Name $intent.name -PolicyStore ActiveStore -NetworkProfile All -Precedence 254 -AppPathNameMatchCondition ([string]$intent.exe) -DSCPAction ([sbyte]$intent.dscp) -ErrorAction Stop
        $existing = @(Get-NetQosPolicy -PolicyStore ActiveStore -ErrorAction Stop | Where-Object { $_.Name -ieq $intent.name })
        }
        if ($existing.Count -ne 1 -or -not (Matches $existing[0] $intent)) { throw ('Configuração DSCP não confirmada: ' + $intent.name) }
        $intent.confirmed = $true
    }
} catch { $errorText = $_.Exception.Message }
@{ owned = @($owned); error = $errorText } | ConvertTo-Json -Depth 12 -Compress
