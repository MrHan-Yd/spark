# Spark native plugin e2e (pipe-level, ASCII only).
# Verifies: list -> open -> lazy-spawn -> plugin.page rpc -> page_closed notification
#           -> re-spawn -> page_closed(id request) -> query without native traces.
$ErrorActionPreference = 'Stop'

$pipeName = 'spark.host.ipc'
$failures = 0

function Send-Recv {
    param($writer, $reader, $json, [int]$timeoutSec = 12)
    $writer.WriteLine($json)
    $t = $reader.ReadLineAsync()
    if (-not $t.Wait([TimeSpan]::FromSeconds($timeoutSec))) {
        throw "read timeout (${timeoutSec}s) for: $json"
    }
    if ($null -eq $t.Result) { throw "pipe closed (null line) for: $json" }
    return $t.Result
}

function Wait-ProcessGone {
    param([string]$name, [int]$maxSec = 6)
    for ($i = 0; $i -lt ($maxSec * 2); $i++) {
        if (-not (Get-Process -Name $name -ErrorAction SilentlyContinue)) { return $true }
        Start-Sleep -Milliseconds 500
    }
    return $false
}

$pipe = New-Object System.IO.Pipes.NamedPipeClientStream('.', $pipeName, [System.IO.Pipes.PipeDirection]::InOut)
$connected = $false
for ($i = 0; $i -lt 10 -and -not $connected; $i++) {
    try { $pipe.Connect(1500); $connected = $true } catch { Start-Sleep -Milliseconds 400 }
}
if (-not $connected) { throw "cannot connect to $pipeName (host not listening?)" }

$enc = New-Object System.Text.UTF8Encoding($false)
$writer = New-Object System.IO.StreamWriter($pipe, $enc, 65536)
$writer.AutoFlush = $true
$writer.NewLine = "`n"
$reader = New-Object System.IO.StreamReader($pipe, [System.Text.Encoding]::UTF8, $false, 65536, $true)

# -- 1. plugin.list: echo present, enabled, has_page
$r1 = Send-Recv $writer $reader '{"jsonrpc":"2.0","id":1,"method":"host.plugin.list","params":{}}'
$l1 = $r1 | ConvertFrom-Json
$echo = $l1.result | Where-Object { $_.id -eq 'com.spark.echo' }
if (-not $echo) { throw "FAIL list: com.spark.echo missing in: $r1" }
if (-not $echo.enabled) {
    $rt = Send-Recv $writer $reader '{"jsonrpc":"2.0","id":10,"method":"host.plugin.toggle","params":{"id":"com.spark.echo","enabled":true}}'
    if ($rt -notmatch '"ok":true') { throw "FAIL toggle enable: $rt" }
    $r1b = Send-Recv $writer $reader '{"jsonrpc":"2.0","id":11,"method":"host.plugin.list","params":{}}'
    $echo = ($r1b | ConvertFrom-Json).result | Where-Object { $_.id -eq 'com.spark.echo' }
    if (-not $echo.enabled) { throw "FAIL list: echo still not enabled" }
}
if (-not $echo.has_page) { throw "FAIL list: has_page not true for echo: $r1" }
Write-Output 'PASS 1/7 list: echo enabled with has_page=true'

# -- 2. plugin.open: native page model info
$r2 = Send-Recv $writer $reader '{"jsonrpc":"2.0","id":2,"method":"host.plugin.open","params":{"id":"com.spark.echo"}}'
$l2 = $r2 | ConvertFrom-Json
$mainAbs = $l2.result.main_abs
if (-not $mainAbs -or $mainAbs -notlike '*page.html') {
    throw "FAIL open: main_abs should be page path, got: $r2"
}
Write-Output "PASS 2/7 open: main_abs=$mainAbs"

# -- 3. plugin.api rpc: lazy spawn + handshake + plugin.page round-trip
$r3 = Send-Recv $writer $reader '{"jsonrpc":"2.0","id":3,"method":"host.plugin.api","params":{"plugin_id":"com.spark.echo","capability":"rpc","method":"echo","args":{"k":"v","n":7}}}' 25
if ($r3 -notmatch '"echoed_method":"echo"') { throw "FAIL rpc: $r3" }
if ($r3 -notmatch '"k":"v"') { throw "FAIL rpc args: $r3" }
Write-Output 'PASS 3/7 rpc: plugin.page round-trip (lazy spawn + handshake)'

# -- 4. lazy-spawn side effect: exe process running
$p = Get-Process -Name 'spark-plugin-echo' -ErrorAction SilentlyContinue
if (-not $p) { throw 'FAIL spawn: spark-plugin-echo.exe not running after rpc' }
Write-Output "PASS 4/7 lazy-spawn: spark-plugin-echo.exe running (pid=$($p.Id))"

# -- 5. page_closed as NOTIFICATION (no id): no reply expected, exe must exit
$writer.WriteLine('{"jsonrpc":"2.0","method":"host.plugin.page_closed","params":{"id":"com.spark.echo"}}')
if (-not (Wait-ProcessGone 'spark-plugin-echo' 8)) { throw 'FAIL page_closed(notification): exe still running' }
Write-Output 'PASS 5/7 page_closed notification: exe exited, no reply needed'

# -- 6. rpc again: re-spawn path works after shutdown
$r6 = Send-Recv $writer $reader '{"jsonrpc":"2.0","id":6,"method":"host.plugin.api","params":{"plugin_id":"com.spark.echo","capability":"rpc","method":"echo","args":{"n":1}}}' 25
if ($r6 -notmatch '"echoed_method":"echo"') { throw "FAIL respawn rpc: $r6" }
$p6 = Get-Process -Name 'spark-plugin-echo' -ErrorAction SilentlyContinue
if (-not $p6) { throw 'FAIL respawn: exe not running again' }
Write-Output 'PASS 6/7 respawn: rpc works and exe re-spawned'

# -- 7. page_closed WITH id (legacy request style): {ok} reply + exe exit
$r7 = Send-Recv $writer $reader '{"jsonrpc":"2.0","id":7,"method":"host.plugin.page_closed","params":{"id":"com.spark.echo"}}'
if ($r7 -notmatch '"ok":true') { throw "FAIL page_closed(id): $r7" }
if (-not (Wait-ProcessGone 'spark-plugin-echo' 8)) { throw 'FAIL page_closed(id): exe still running' }
Write-Output 'PASS 7/7 page_closed(id): {ok} reply, exe exited'

# -- 8. query smoke: native echo keyword candidate present and opens page route
$r8 = Send-Recv $writer $reader '{"jsonrpc":"2.0","id":8,"method":"host.query","params":{"text":"echo","limit":20}}'
if ($r8 -notmatch '"plugin:com\.spark\.echo:echo"') { throw "FAIL query: native echo keyword candidate missing: $r8" }
if ($r8 -notmatch '"plugin:page:com\.spark\.echo"') { throw "FAIL query: native echo target not page route: $r8" }
Write-Output 'PASS 8/7 query: native echo keyword candidate present (page route)'

Write-Output 'E2E_ALL_PASS'