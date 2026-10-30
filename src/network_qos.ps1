$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
[Console]::OutputEncoding = New-Object System.Text.UTF8Encoding($false)
function Field($p, $names) {
    foreach ($name in $names) {
        $prop = $p.CimInstanceProperties | Where-Object { $_.Name -eq $name } | Select-Object -First 1
        if ($null -ne $prop) { return $prop.Value }
        $prop = $p.PSObject.Properties[$name]
        if ($null -ne $prop) { return $prop.Value }
    }
    return $null
}
function Proof($p) {
    $properties = @($p.CimInstanceProperties | Sort-Object Name | ForEach-Object { ,@($_.Name, $_.Value) })
    if ($properties.Count -eq 0) { throw 'NetQos CIM schema unavailable; refusing unproved mutation' }
    return ,$properties
}
function App($p) { return Field $p @('AppPathNameMatchCondition', 'AppPathName') }
function Rate($p) {
    $value = Field $p @('ThrottleRateActionBitsPerSecond', 'ThrottleRateAction', 'ThrottleRate')
    if ($null -eq $value) { throw 'NetQos raw throttle field unavailable; refusing to infer formatted rate' }
    if ([string]$value -eq '-1') { return [UInt64]::MaxValue }
    return [UInt64]$value
}
function Dscp($p) {
    $v = Field $p @('DSCPAction', 'DSCPValue')
    if ($null -eq $v -or [string]$v -eq '-1' -or [string]$v -eq '4294967295') { return -1 }
    $n = [int]$v
    if ($n -lt 0 -or $n -gt 63) { throw 'Ambiguous existing DSCP; refusing to change it' }
    return $n
}
function NoExtraConditions($p) {
    foreach ($property in $p.CimInstanceProperties) {
        if ($property.Name -match '(Condition|Match)$' -and $property.Name -notmatch '^AppPathName') {
            $v = [string]$property.Value
            if ($v -notin @('', '0', '-1', '256', '65535', '4294967295', 'Any', 'Both')) { throw ('Unsupported existing QoS condition: ' + $property.Name) }
        }
        if ($property.Name -match 'Action$' -and $property.Name -notmatch '^(DSCP|ThrottleRate)') {
            if ([string]$property.Value -notin @('', '0', '-1', '4294967295')) { throw ('Unexpected QoS action: ' + $property.Name) }
        }
    }
}
try {
    $request = [Console]::In.ReadToEnd() | ConvertFrom-Json
    $name = [string]$request.name
    $exe = [string]$request.exe
    $bps = [UInt64]::Parse([string]$request.bps, [Globalization.CultureInfo]::InvariantCulture)
    if ($name -notmatch '^RintelRAPO-Rate-[0-9]+-[0-9]+$' -or -not [IO.Path]::IsPathRooted($exe) -or [IO.Path]::GetExtension($exe) -ine '.exe' -or $bps -eq 0) { throw 'Invalid policy identity or rate' }
    $all = @(Get-NetQosPolicy -PolicyStore ActiveStore -ErrorAction Stop)
    $own = @($all | Where-Object { $_.Name -ceq $name })
    if ($own.Count -gt 1) { throw 'Duplicate policy identity; refusing mutation' }
    if ($request.action -in @('prepare', 'create')) {
        if ($own.Count -ne 0) { throw 'Policy already exists; never adopting or overwriting it' }
        if (-not [IO.File]::Exists($exe)) { throw 'Executable no longer exists' }
        $dscp = -1
        foreach ($policy in $all) {
            $app = [string](App $policy)
            if ($app -ieq $exe -or $app -ieq [IO.Path]::GetFileName($exe) -or $app -eq '' -or $app -eq '*') {
                if ($app -eq '' -or $app -eq '*') { throw 'Global/wildcard QoS policy is ambiguous; existing policies preserved' }
                NoExtraConditions $policy
                $existingRate = Rate $policy
                if ($existingRate -ne 0 -and $existingRate -ne [UInt64]::MaxValue) { throw 'Existing throttle policy targets this executable; preserved' }
                $value = Dscp $policy
                if ($value -ge 0) {
                    if ($dscp -ge 0 -and $dscp -ne $value) { throw 'Conflicting DSCP policies; preserved' }
                    $dscp = $value
                }
                $precedence = Field $policy @('Precedence')
                if ($null -ne $precedence -and [int]$precedence -ge 255) { throw 'Existing precedence 255; cannot preserve effective DSCP safely' }
            }
        }
        if ($request.action -eq 'prepare') {
            @{ ok = $true; dscp = $dscp } | ConvertTo-Json -Compress
            exit 0
        }
        if ($null -eq $request.dscp -or [int]$request.dscp -ne $dscp) { throw 'DSCP state changed since preparation; existing policy preserved' }
        $parameters = @{ Name = $name; PolicyStore = 'ActiveStore'; NetworkProfile = 'All'; Precedence = 255; AppPathNameMatchCondition = $exe; ThrottleRateActionBitsPerSecond = $bps; ErrorAction = 'Stop' }
        if ($dscp -ge 0) { $parameters.DSCPAction = $dscp }
        $null = New-NetQosPolicy @parameters
        $own = @(Get-NetQosPolicy -PolicyStore ActiveStore -ErrorAction Stop | Where-Object { $_.Name -ceq $name })
        if ($own.Count -ne 1 -or (App $own[0]) -ine $exe -or (Rate $own[0]) -ne $bps -or (Dscp $own[0]) -ne $dscp) { throw 'Policy creation not confirmed; keep session intent for cleanup' }
        @{ ok = $true; proof = (Proof $own[0]) } | ConvertTo-Json -Depth 16 -Compress
    } elseif ($request.action -in @('validate', 'remove')) {
        if ($own.Count -eq 0) {
            if ($request.action -eq 'validate') { throw 'Owned policy disappeared externally' }
            @{ ok = $true } | ConvertTo-Json -Compress
        } else {
            $p = $own[0]
            if ((App $p) -ine $exe -or (Rate $p) -ne $bps) { throw 'Owned policy identity/actions changed externally; preserved' }
            if ($null -ne $request.proof) {
                $current = ConvertTo-Json -InputObject (Proof $p) -Depth 16 -Compress
                $expected = ConvertTo-Json -InputObject $request.proof -Depth 16 -Compress
                if ($current -cne $expected) { throw 'Owned policy attributes changed externally; preserved' }
            } else {
                # An interrupted create has only its pre-recorded session intent. Never sweep by prefix.
                NoExtraConditions $p
                if ([string](Field $p @('NetworkProfile')) -notin @('All', '7') -or [int](Field $p @('Precedence')) -ne 255) { throw 'Interrupted create has unexpected attributes; preserved for inspection' }
                if ($null -eq $request.dscp -or (Dscp $p) -ne [int]$request.dscp) { throw 'Interrupted policy DSCP changed; preserved' }
            }
            if ($request.action -eq 'remove') { Remove-NetQosPolicy -Name $name -PolicyStore ActiveStore -Confirm:$false -ErrorAction Stop }
            @{ ok = $true; proof = (Proof $p) } | ConvertTo-Json -Depth 16 -Compress
        }
    } else { throw 'Unknown policy operation' }
} catch {
    @{ ok = $false; error = $_.Exception.Message } | ConvertTo-Json -Compress
    exit 1
}
