# Run in elevated Windows PowerShell on a disposable guest with a dedicated engine.
[CmdletBinding()]
param(
    [Parameter(Mandatory)][switch]$DisposableGuest,
    [Parameter(Mandatory)][string]$DsgdExe,
    [Parameter(Mandatory)][ValidatePattern('^\\\\\.\\pipe\\[A-Za-z0-9][A-Za-z0-9_.-]{0,246}\z')][string]$UpstreamPipe,
    [Parameter(Mandatory)][ValidatePattern('^S-1-[0-9]+(-[0-9]+){1,15}\z')][string]$UpstreamSid,
    [Parameter(Mandatory)][ValidatePattern('^[A-Za-z0-9][A-Za-z0-9_.:-]{0,127}\z')][string]$EngineId,
    [Parameter(Mandatory)][ValidateSet('Real', 'Fixture')][string]$EngineKind
)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
if (!$DisposableGuest -or [Environment]::OSVersion.Platform -ne 'Win32NT') { throw 'DSG_WINDOWS_GUEST_REQUIRED' }
$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$admin = [Security.Principal.WindowsPrincipal]::new($identity)
if (!$admin.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) { throw 'DSG_ADMIN_REQUIRED' }
if ($DsgdExe -notmatch '^[A-Za-z]:\\' -or $DsgdExe -match '["%\x00-\x1f]' -or
    [IO.Path]::GetFileName($DsgdExe) -ine 'dsgd.exe' -or ![IO.File]::Exists($DsgdExe) -or
    $UpstreamPipe.EndsWith('.') -or $UpstreamSid -in @('S-1-5-19', 'S-1-5-20')) { throw 'DSG_INVALID_INPUT' }

$name = 'dsg-check-' + [Guid]::NewGuid().ToString('N')
$listener = '\\.\pipe\' + $name
$command = '"{0}" --listen {1} --upstream {2} --engine-id {3} --service-name {4} --allow-sid S-1-5-19 --upstream-sid {5}' -f
    [IO.Path]::GetFullPath($DsgdExe), $listener, $UpstreamPipe, $EngineId, $name, $UpstreamSid
$created = $false
$stage = 'create'
$failure = $null

function Get-OwnedService {
    $svc = Get-CimInstance Win32_Service -Property Name,DisplayName,PathName,State,ProcessId -Filter "Name='$name'" -OperationTimeoutSec 6
    if ($null -eq $svc -or $svc.PathName -cne $command -or $svc.DisplayName -cne $name) {
        throw 'DSG_SERVICE_OWNERSHIP_UNKNOWN'
    }
    $svc
}

function Stop-OwnedServiceProcess {
    $svc = Get-OwnedService
    if ($svc.State -ne 'Running' -or $svc.ProcessId -eq 0) { throw 'DSG_PROCESS_OWNERSHIP_UNKNOWN' }
    $process = [Diagnostics.Process]::GetProcessById([int]$svc.ProcessId)
    try {
        $null = $process.Handle
        if ($process.MainModule.FileName -ine [IO.Path]::GetFullPath($DsgdExe) -or
            (Get-OwnedService).ProcessId -ne $svc.ProcessId) { throw 'DSG_PROCESS_OWNERSHIP_UNKNOWN' }
        $process.Kill()
        if (!$process.WaitForExit(6000)) { throw 'DSG_CRASH_WAIT_TIMEOUT' }
    } finally { $process.Dispose() }
    $timer = [Diagnostics.Stopwatch]::StartNew()
    do {
        if ((Get-OwnedService).State -eq 'Stopped') { return }
        Start-Sleep -Milliseconds 200
    } while ($timer.Elapsed.TotalSeconds -lt 8)
    throw 'DSG_SCM_CRASH_STATE_TIMEOUT'
}

function Set-ServiceState([string]$Method, [string]$State) {
    $svc = Get-OwnedService
    if ($svc.State -eq $State) { return }
    $result = Invoke-CimMethod -InputObject $svc -MethodName $Method -OperationTimeoutSec 8
    if ($result.ReturnValue -ne 0) { throw 'DSG_SCM_CONTROL_FAILED' }
    $timer = [Diagnostics.Stopwatch]::StartNew()
    do {
        if ((Get-OwnedService).State -eq $State) { return }
        Start-Sleep -Milliseconds 200
    } while ($timer.Elapsed.TotalSeconds -lt 12)
    throw 'DSG_SCM_STATE_TIMEOUT'
}

# Run inside a scheduler-owned process. Its execution time limit also bounds synchronous pipe I/O.
$probe = {
    $ErrorActionPreference = 'Stop'
    Set-StrictMode -Version Latest
    function Open-Pipe([string]$PipeName) {
        $pipe = [IO.Pipes.NamedPipeClientStream]::new('.', $PipeName, [IO.Pipes.PipeAccessRights]0x100003,
            [IO.Pipes.PipeOptions]::None, [Security.Principal.TokenImpersonationLevel]::Identification,
            [IO.HandleInheritability]::None)
        try { $pipe.Connect(2500) } catch { $pipe.Dispose(); throw }
        return $pipe
    }
    function Assert-Denied([string]$PipeName) {
        try { $pipe = Open-Pipe $PipeName }
        catch {
            if ($_.Exception.GetBaseException() -is [UnauthorizedAccessException]) { return }
            throw 'DSG_DENIAL_UNCONFIRMED'
        }
        $pipe.Dispose()
        throw 'DSG_BOUNDARY_OPEN'
    }
    function Read-Route([string]$Path, [int]$Status) {
        $pipe = Open-Pipe $listenName
        try {
            $request = [Text.Encoding]::ASCII.GetBytes("GET $Path HTTP/1.0`r`n`r`n")
            $pipe.Write($request, 0, $request.Length)
            $buffer = [byte[]]::new(4096)
            $used = 0
            while ($used -lt $buffer.Length) {
                $count = $pipe.Read($buffer, $used, $buffer.Length - $used)
                if (!$count) { break }
                $used += $count
                $raw = [Text.Encoding]::ASCII.GetString($buffer, 0, $used)
                $end = $raw.IndexOf("`r`n`r`n")
                if ($end -lt 0) { continue }
                $head = $raw.Substring(0, $end)
                if (!$head.StartsWith("HTTP/1.1 $Status ") -or $head -notmatch '(?im)^Content-Length: ([0-9]+)\r?$') {
                    throw 'DSG_HTTP_FRAMING'
                }
                $length = [int]$Matches[1]
                if ($used -eq $end + 4 + $length) { return $raw.Substring($end + 4) }
            }
            throw 'DSG_RESPONSE_INCOMPLETE'
        } finally { $pipe.Dispose() }
    }
    try {
        if ([Security.Principal.WindowsIdentity]::GetCurrent().User.Value -ne $sid) { exit 10 }
        if ($mode -eq 'denied') { Assert-Denied $listenName; exit 0 }
        if ($mode -eq 'full') { Assert-Denied $upName }
        if ((Read-Route '/_ping' 200) -cne 'OK') { exit 11 }
        if ($mode -eq 'restart') { exit 0 }
        $info = (Read-Route '/info' 200) | ConvertFrom-Json
        if ($info.ID -cne $expected -or @($info.PSObject.Properties).Count -ne 1) { exit 12 }
        $inventory = (Read-Route '/dsg/v1/inventory' 200) | ConvertFrom-Json
        if ($inventory.engine_id -cne $expected -or $inventory.status -cne 'unknown' -or
            $inventory.complete -isnot [bool] -or $inventory.complete -or
            $null -ne $inventory.volume_count -or $null -ne $inventory.duplicate_groups) { exit 13 }
        foreach ($value in @($inventory.container_count, $inventory.image_count)) {
            if (($value -isnot [int] -and $value -isnot [long]) -or $value -lt 0) { exit 14 }
        }
        foreach ($key in @('containers', 'images', 'volumes', 'duplicates')) {
            if ($null -ne $inventory.$key) { exit 15 }
        }
        $null = Read-Route '/unsupported' 403
        exit 0
    } catch { exit 16 }
}

function Invoke-Probe([string]$Mode, [string]$Sid) {
    $taskName = "$name-$Mode"
    # Values embedded in the child script are generated here or restricted by the parameter grammar.
    $prefix = "`$mode='$Mode';`$sid='$Sid';`$listenName='$name';`$upName='$($UpstreamPipe.Substring(9))';`$expected='$EngineId';"
    $encoded = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($prefix + $probe.ToString()))
    $exe = Join-Path ([Environment]::SystemDirectory) 'WindowsPowerShell\v1.0\powershell.exe'
    $arguments = "-NoLogo -NoProfile -NonInteractive -EncodedCommand $encoded"
    $action = New-ScheduledTaskAction -Execute $exe -Argument $arguments
    $principal = New-ScheduledTaskPrincipal -UserId $Sid -LogonType ServiceAccount -RunLevel Limited
    $settings = New-ScheduledTaskSettingsSet -ExecutionTimeLimit (New-TimeSpan -Seconds 30) -AllowStartIfOnBatteries
    $null = Register-ScheduledTask -TaskPath '\' -TaskName $taskName -Action $action -Settings $settings -Principal $principal
    function Get-OwnedTask {
        $task = Get-ScheduledTask -TaskPath '\' -TaskName $taskName
        if (@($task.Actions).Count -ne 1 -or $task.Actions[0].Execute -cne $exe -or
            $task.Actions[0].Arguments -cne $arguments) { throw 'DSG_TASK_OWNERSHIP_UNKNOWN' }
        $task
    }
    try {
        $before = (Get-ScheduledTaskInfo -InputObject (Get-OwnedTask)).LastRunTime
        Start-ScheduledTask -InputObject (Get-OwnedTask)
        $timer = [Diagnostics.Stopwatch]::StartNew()
        do {
            $task = Get-OwnedTask
            $info = Get-ScheduledTaskInfo -InputObject $task
            if ($task.State -eq 'Ready' -and $info.LastRunTime -gt $before -and $info.LastTaskResult -ne 267009) {
                if ($info.LastTaskResult -ne 0) { throw 'DSG_PROBE_FAILED' }
                return
            }
            Start-Sleep -Milliseconds 200
        } while ($timer.Elapsed.TotalSeconds -lt 35)
        throw 'DSG_PROBE_TIMEOUT'
    } finally {
        $task = Get-OwnedTask
        if ($task.State -in @('Running', 'Queued')) {
            Stop-ScheduledTask -InputObject $task
            $wait = [Diagnostics.Stopwatch]::StartNew()
            do {
                $task = Get-OwnedTask
                if ($task.State -notin @('Running', 'Queued')) { break }
                Start-Sleep -Milliseconds 200
            } while ($wait.Elapsed.TotalSeconds -lt 6)
            if ($task.State -in @('Running', 'Queued')) { throw 'DSG_TASK_STOP_UNKNOWN' }
        }
        Unregister-ScheduledTask -InputObject (Get-OwnedTask) -Confirm:$false
        if (Get-CimInstance -Namespace 'root/Microsoft/Windows/TaskScheduler' -ClassName MSFT_ScheduledTask -Property TaskName -Filter "TaskName='$taskName'" -OperationTimeoutSec 6) {
            throw 'DSG_TASK_REMAINS'
        }
    }
}
try {
    $result = Invoke-CimMethod -ClassName Win32_Service -MethodName Create -OperationTimeoutSec 8 -Arguments @{
        Name = $name; DisplayName = $name; PathName = $command; ServiceType = [byte]16
        ErrorControl = [byte]1; StartMode = 'Manual'; DesktopInteract = $false; StartName = 'LocalSystem'
    }
    if ($result.ReturnValue -ne 0) { throw 'DSG_SCM_CREATE_FAILED' }
    $created = $true
    $stage = 'start'
    Set-ServiceState StartService Running
    $stage = 'allowed-boundary-and-projection'
    Invoke-Probe full S-1-5-19
    $stage = 'denied-boundary'
    Invoke-Probe denied S-1-5-20
    $stage = 'normal-stop-restart'
    Set-ServiceState StopService Stopped
    Set-ServiceState StartService Running
    Invoke-Probe restart S-1-5-19
    $stage = 'crash-restart'
    Stop-OwnedServiceProcess
    Set-ServiceState StartService Running
    Invoke-Probe restart S-1-5-19
} catch {
    $failure = $stage
} finally {
    if ($created) {
        try {
            Set-ServiceState StopService Stopped
            $result = Invoke-CimMethod -InputObject (Get-OwnedService) -MethodName Delete -OperationTimeoutSec 8
            if ($result.ReturnValue -ne 0) { throw 'DSG_SCM_DELETE_FAILED' }
            $timer = [Diagnostics.Stopwatch]::StartNew()
            while (Get-CimInstance Win32_Service -Property Name -Filter "Name='$name'" -OperationTimeoutSec 6) {
                if ($timer.Elapsed.TotalSeconds -gt 8) { throw 'DSG_SCM_DELETE_TIMEOUT' }
                Start-Sleep -Milliseconds 200
            }
        } catch { $failure = "cleanup-after-$stage" }
    }
}
if ($failure) { throw "DSG_WINDOWS_CHECK_FAILED stage=$failure resource=$name" }
Write-Output "DSG_WINDOWS_CHECK_PASS engine_kind=$EngineKind lifecycle=normal-and-crash-restart accounts=2 temporary_resources=removed"
