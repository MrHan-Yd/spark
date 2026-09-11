# e2e: regex/root triggers + fs scopes + shell validation + readImage (host pipe, ASCII only)
$ErrorActionPreference = 'Stop'
# Repo root = two levels above this script (scripts/e2e/*.ps1).
$root = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$hostExeSrc = Join-Path $root 'target\debug\spark-host.exe'

$tmp = Join-Path $env:TEMP ('spark_feat_e2e_' + [guid]::NewGuid().ToString('N'))
$hostDir = Join-Path $tmp 'host'
$pluginsDir = Join-Path $tmp 'plugins'
$devPlugin = Join-Path $tmp 'devplugin'
$scopeDir = Join-Path $tmp 'scopedir'
New-Item -ItemType Directory -Path (Join-Path $hostDir 'data'), $pluginsDir, $devPlugin, $scopeDir -Force | Out-Null
Copy-Item $hostExeSrc (Join-Path $hostDir 'spark-host.exe')

$enc = New-Object System.Text.UTF8Encoding($false)
$features = '[{"type":"regex","pattern":"^\\d{1,3}(\\.\\d{1,3}){3}$","title":"IP","mode":"page"},{"type":"root","title":"RootEntry","mode":"page"}]'
$manifest = '{"id":"com.spark.feat.e2e","name":"FE","version":"0.1.0","api_version":2,"runtime":"webview","main":"index.html","permissions":["fs.read","fs.write","shell.open","clipboard"],"features":' + $features + '}'
[System.IO.File]::WriteAllText((Join-Path $devPlugin 'plugin.json'), $manifest, $enc)
[System.IO.File]::WriteAllText((Join-Path $devPlugin 'index.html'), '<html></html>', $enc)

$proc = Start-Process -FilePath (Join-Path $hostDir 'spark-host.exe') -ArgumentList @('--no-ui', '--plugins-dir', $pluginsDir) -WindowStyle Hidden -PassThru
Start-Sleep -Seconds 3

$enc = New-Object System.Text.UTF8Encoding($false)
$pipe = New-Object System.IO.Pipes.NamedPipeClientStream('.', 'spark.host.ipc', [System.IO.Pipes.PipeDirection]::InOut)
$pipe.Connect(5000)
$writer = New-Object System.IO.StreamWriter($pipe, $enc); $writer.AutoFlush = $true; $writer.NewLine = "`n"
$reader = New-Object System.IO.StreamReader($pipe, $enc)
$script:id = 0
function Invoke-Rpc([string]$method, $params) {
  $script:id++
  $req = @{ jsonrpc = '2.0'; id = $script:id; method = $method; params = $params } | ConvertTo-Json -Compress -Depth 8
  $writer.WriteLine($req)
  $line = $reader.ReadLine()
  if ($null -eq $line) { throw 'host closed pipe' }
  return ($line | ConvertFrom-Json)
}

$failures = New-Object System.Collections.Generic.List[string]

$dl = Invoke-Rpc 'host.plugin.devload' @{ dir = $devPlugin }
if (-not $dl.result.id) { $failures.Add('devload failed') }
$gr = Invoke-Rpc 'host.plugin.grant' @{ id = 'com.spark.feat.e2e'; permissions = @('fs.read','fs.write','shell.open','clipboard'); fs_scopes = @{ 'fs.read' = @($scopeDir); 'fs.write' = @($scopeDir) } }
if (-not $gr.result.ok) { $failures.Add('grant failed') }

# regex trigger: candidate appended with full input + empty command
$q1 = Invoke-Rpc 'host.query' @{ text = '192.168.1.1'; limit = 50 }
$ipHit = $q1.result.items | Where-Object { $_.plugin_id -eq 'com.spark.feat.e2e' }
if (-not $ipHit) { $failures.Add('regex trigger: no candidate') }
else {
  if ($ipHit.plugin_input -ne '192.168.1.1') { $failures.Add('regex plugin_input=' + $ipHit.plugin_input) }
  if ($ipHit.plugin_command -ne '') { $failures.Add('regex plugin_command=' + $ipHit.plugin_command) }
}

# root trigger on generic query
$q2 = Invoke-Rpc 'host.query' @{ text = 'anything else'; limit = 50 }
$rootHit = $q2.result.items | Where-Object { $_.plugin_id -eq 'com.spark.feat.e2e' }
if (-not $rootHit) { $failures.Add('root trigger: no candidate') }
else {
  if ($rootHit.plugin_input -ne 'anything else') { $failures.Add('root plugin_input=' + $rootHit.plugin_input) }
  if ($rootHit.plugin_command -ne '') { $failures.Add('root plugin_command=' + $rootHit.plugin_command) }
}

# fs read inside/outside scope
[System.IO.File]::WriteAllText((Join-Path $scopeDir 'a.txt'), 'hello-fs', $enc)
$rin = Invoke-Rpc 'host.plugin.api' @{ plugin_id='com.spark.feat.e2e'; capability='fs'; method='read'; args = @{ path = (Join-Path $scopeDir 'a.txt') } }
if ($rin.result.data.text -ne 'hello-fs') { $failures.Add('fs read inside: ' + ($rin | ConvertTo-Json -Compress -Depth 6)) }
$outPath = Join-Path $tmp 'outside.txt'
[System.IO.File]::WriteAllText($outPath, 'secret', $enc)
$rout = Invoke-Rpc 'host.plugin.api' @{ plugin_id='com.spark.feat.e2e'; capability='fs'; method='read'; args = @{ path = $outPath } }
if (-not $rout.error -or $rout.error.message -notmatch 'PERMISSION_SCOPE') { $failures.Add('fs read outside not scoped') }

# fs write inside scope (nested new dir)
$w = Invoke-Rpc 'host.plugin.api' @{ plugin_id='com.spark.feat.e2e'; capability='fs'; method='write'; args = @{ path = (Join-Path $scopeDir 'sub\new.txt'); text = 'written' } }
if (-not $w.result.data.ok) { $failures.Add('fs write inside failed: ' + ($w | ConvertTo-Json -Compress -Depth 6)) }
if (([System.IO.File]::ReadAllText((Join-Path $scopeDir 'sub\new.txt'))) -ne 'written') { $failures.Add('fs write content mismatch') }

# clipboard read_image: data null or base64, no error
$ci = Invoke-Rpc 'host.plugin.api' @{ plugin_id='com.spark.feat.e2e'; capability='clipboard'; method='read_image'; args = @{} }
if ($ci.error) { $failures.Add('read_image errored: ' + $ci.error.message) }

# shell open_external validation only (control char target rejected without launching)
$badTarget = 'x' + [char]0x000A + 'y'
$sh = Invoke-Rpc 'host.plugin.api' @{ plugin_id='com.spark.feat.e2e'; capability='shell'; method='open_external'; args = @{ target = $badTarget } }
if (-not $sh.error -or $sh.error.message -notmatch 'INVALID_ARGS') { $failures.Add('shell control-char not rejected') }

# list exposes fs_scopes
$lst = Invoke-Rpc 'host.plugin.list' @{}
$fp = $lst.result | Where-Object { $_.id -eq 'com.spark.feat.e2e' }
if (-not $fp.fs_scopes -or -not $fp.fs_scopes.'fs.read') { $failures.Add('list fs_scopes missing') }

$writer.Dispose(); $reader.Dispose(); $pipe.Dispose()
$proc.Kill(); $proc.WaitForExit(5000) | Out-Null

if ($failures.Count -gt 0) {
  Write-Output 'E2E FAILED:'
  foreach ($f in $failures) { Write-Output ('  - ' + $f) }
  exit 1
}
Write-Output 'E2E PASSED: regex+root trigger / fs scope read+write / scope denial / shell validation / list fs_scopes'