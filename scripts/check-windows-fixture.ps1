# Windows PowerShell 5.1, in a disposable guest under LocalSystem.
[CmdletBinding()]
param(
    [Parameter(Mandatory)][switch]$DisposableGuest,
    [Parameter(Mandatory)][string]$DsgdExe
)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
if (!$DisposableGuest -or [Environment]::OSVersion.Platform -ne 'Win32NT') { throw 'DSG_WINDOWS_GUEST_REQUIRED' }
if (![Security.Principal.WindowsIdentity]::GetCurrent().IsSystem) { throw 'DSG_FIXTURE_SYSTEM_REQUIRED' }

$name = 'dsg-fixture-' + [Guid]::NewGuid().ToString('N')
$ready = [Threading.ManualResetEvent]::new($false)
$stop = [Threading.ManualResetEvent]::new($false)
$server = [PowerShell]::Create()
$serve = {
    param($name, $ready, $stop)
    $ErrorActionPreference = 'Stop'
    $system = [Security.Principal.SecurityIdentifier]::new('S-1-5-18')
    $network = [Security.Principal.SecurityIdentifier]::new('S-1-5-2')
    $acl = [IO.Pipes.PipeSecurity]::new()
    $acl.SetAccessRuleProtection($true, $false)
    $acl.SetOwner($system)
    $acl.AddAccessRule([IO.Pipes.PipeAccessRule]::new($network, 'FullControl', 'Deny'))
    $acl.AddAccessRule([IO.Pipes.PipeAccessRule]::new($system, 'FullControl', 'Allow'))
    $expected = "GET /info HTTP/1.0`r`nHost: localhost`r`nConnection: close`r`n`r`n"
    $body = '{"ID":"dsg-fixture-engine","Containers":2,"Images":3}'
    $response = [Text.Encoding]::ASCII.GetBytes("HTTP/1.0 200 OK`r`nContent-Length: $($body.Length)`r`nConnection: close`r`n`r`n$body")
    # The four independently scheduled probes and two SCM restarts can outlast one request limit.
    $lifetimeMilliseconds = 140000
    $timer = [Diagnostics.Stopwatch]::StartNew()
    $pipe = [IO.Pipes.NamedPipeServerStream]::new($name, 'InOut', 1, 'Byte', 'Asynchronous', 4096, 4096, $acl)
    try {
        for ($count = 0; $count -lt 16; $count++) {
            $accept = $pipe.BeginWaitForConnection($null, $null)
            $null = $ready.Set()
            $left = [Math]::Max(0, $lifetimeMilliseconds - [int]$timer.ElapsedMilliseconds)
            $wait = [Threading.WaitHandle]::WaitAny(@($stop, $accept.AsyncWaitHandle), $left)
            if ($wait -eq 0) { return }
            if ($wait -ne 1) { throw 'DSG_FIXTURE_LIFETIME_LIMIT' }
            $pipe.EndWaitForConnection($accept)
            $deadline = [Math]::Min($lifetimeMilliseconds, $timer.ElapsedMilliseconds + 2000)
            $buffer = [byte[]]::new($expected.Length)
            $used = 0
            while ($used -lt $buffer.Length) {
                $read = $pipe.ReadAsync($buffer, $used, $buffer.Length - $used)
                if (!$read.Wait([int][Math]::Max(0, $deadline - $timer.ElapsedMilliseconds))) { throw 'DSG_FIXTURE_READ_TIMEOUT' }
                if ($read.Result -eq 0) { throw 'DSG_FIXTURE_INCOMPLETE_REQUEST' }
                $used += $read.Result
            }
            if ([Text.Encoding]::ASCII.GetString($buffer) -cne $expected) { throw 'DSG_FIXTURE_UNEXPECTED_REQUEST' }
            $write = $pipe.WriteAsync($response, 0, $response.Length)
            if (!$write.Wait([int][Math]::Max(0, $deadline - $timer.ElapsedMilliseconds))) { throw 'DSG_FIXTURE_WRITE_TIMEOUT' }
            # Disconnect only after the client consumed the framed response and closed its end.
            $eof = $pipe.ReadAsync($buffer, 0, 1)
            if (!$eof.Wait([int][Math]::Max(0, $deadline - $timer.ElapsedMilliseconds)) -or $eof.Result -ne 0) {
                throw 'DSG_FIXTURE_CLIENT_CLOSE_FAILED'
            }
            $pipe.Disconnect()
        }
        throw 'DSG_FIXTURE_CONNECTION_LIMIT'
    } finally { $pipe.Dispose() }
}
$job = $null
try {
    $null = $server.AddScript($serve.ToString()).AddArgument($name).AddArgument($ready).AddArgument($stop)
    $job = $server.BeginInvoke()
    if (!$ready.WaitOne(5000)) { throw 'DSG_FIXTURE_START_FAILED' }
    & (Join-Path $PSScriptRoot 'check-windows-service.ps1') -DisposableGuest -DsgdExe $DsgdExe `
        -UpstreamPipe ("\\.\pipe\" + $name) -UpstreamSid 'S-1-5-18' -EngineId 'dsg-fixture-engine' -EngineKind Fixture
} finally {
    $null = $stop.Set()
    try {
        if ($job) {
            if (!$job.AsyncWaitHandle.WaitOne(3000)) { throw 'DSG_FIXTURE_STOP_TIMEOUT' }
            try { $null = $server.EndInvoke($job) }
            catch {
                $cause = $_.Exception.GetBaseException()
                if ($cause.Message -match '^DSG_FIXTURE_[A-Z_]+$') { Write-Output $cause.Message }
                else { Write-Output ('DSG_FIXTURE_EXCEPTION_' + $cause.GetType().Name.ToUpperInvariant()) }
                throw
            }
            if ($server.HadErrors) { throw 'DSG_FIXTURE_SERVER_FAILED' }
        }
    } finally {
        $server.Dispose()
        $stop.Dispose()
        $ready.Dispose()
    }
}
Write-Output 'DSG_FIXTURE_CLOSED engine_kind=Fixture server_processes_started=0'
