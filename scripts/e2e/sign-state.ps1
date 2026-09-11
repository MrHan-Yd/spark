# e2e: plugin signing acceptance (official / tampered / unsigned + startup reverify)
$ErrorActionPreference = 'Stop'
# Repo root = two levels above this script (scripts/e2e/*.ps1).
$root = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$sign = Join-Path $root 'target\release\spark-sign.exe'
$key  = Join-Path $root 'keys\spark-official-v1.key'
$hostExeSrc = Join-Path $root 'target\debug\spark-host.exe'

function New-PluginDir([string]$dir, [string]$id, [string]$kw) {
  New-Item -ItemType Directory -Path $dir -Force | Out-Null
  $enc = New-Object System.Text.UTF8Encoding($false)
  $manifest = '{"id":"' + $id + '","name":"T","version":"0.1.0","api_version":2,"runtime":"webview","main":"index.html","features":[{"type":"keyword","keyword":"' + $kw + '","title":"T","mode":"page"}]}'
  [System.IO.File]::WriteAllText((Join-Path $dir 'plugin.json'), $manifest, $enc)
  [System.IO.File]::WriteAllText((Join-Path $dir 'index.html'), '<html><body>orig</html>', $enc)
}

$tmp = Join-Path $env:TEMP ('spark_sign_e2e_' + [guid]::NewGuid().ToString('N'))
$hostDir = Join-Path $tmp 'host'
$pluginsDir = Join-Path $tmp 'plugins'
New-Item -ItemType Directory -Path (Join-Path $hostDir 'data'), $pluginsDir -Force | Out-Null
Copy-Item $hostExeSrc (Join-Path $hostDir 'spark-host.exe')

New-PluginDir (Join-Path $tmp 'src_official') 'com.spark.sign.official' 'ofc'
& $sign sign --dir (Join-Path $tmp 'src_official') --key $key --key-id spark-official-v1 | Out-Null
if (-not (Test-Path (Join-Path $tmp 'src_official\signature.json'))) { throw 'sign tool produced no signature.json' }

New-PluginDir (Join-Path $tmp 'src_tampered') 'com.spark.sign.tampered' 'tmp'
& $sign sign --dir (Join-Path $tmp 'src_tampered') --key $key --key-id spark-official-v1 | Out-Null
[System.IO.File]::WriteAllText((Join-Path $tmp 'src_tampered\index.html'), '<html>tampered</html>', (New-Object System.Text.UTF8Encoding($false)))

New-PluginDir (Join-Path $tmp 'src_unsigned') 'com.spark.sign.unsigned' 'usg'

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

$r1 = Invoke-Rpc 'host.plugin.install' @{ path = (Join-Path $tmp 'src_official') }
if ($r1.result.action -ne 'installed' -or $r1.result.sign_state -ne 'official') { $failures.Add('official install: ' + $r1.result.sign_state) }

$r2 = Invoke-Rpc 'host.plugin.install' @{ path = (Join-Path $tmp 'src_tampered') }
if ($null -eq $r2.error -or $r2.error.message -notmatch 'signature|Signature') { $failures.Add('tampered install not rejected') }

$r3 = Invoke-Rpc 'host.plugin.install' @{ path = (Join-Path $tmp 'src_unsigned') }
if ($r3.result.action -ne 'installed' -or $r3.result.sign_state -ne 'unsigned') { $failures.Add('unsigned install: ' + $r3.result.sign_state) }

$r4 = Invoke-Rpc 'host.plugin.list' @{}
$off = $r4.result | Where-Object { $_.id -eq 'com.spark.sign.official' }
$usg = $r4.result | Where-Object { $_.id -eq 'com.spark.sign.unsigned' }
if ($off.sign_state -ne 'official') { $failures.Add('list official=' + $off.sign_state) }
if ($usg.sign_state -ne 'unsigned') { $failures.Add('list unsigned=' + $usg.sign_state) }

# startup reverify: tamper the INSTALLED official plugin, restart host -> invalid
$writer.Dispose(); $reader.Dispose(); $pipe.Dispose()
$proc.Kill(); $proc.WaitForExit(5000) | Out-Null
[System.IO.File]::WriteAllText((Join-Path $pluginsDir 'com.spark.sign.official\index.html'), '<html>evil</html>', (New-Object System.Text.UTF8Encoding($false)))
$proc2 = Start-Process -FilePath (Join-Path $hostDir 'spark-host.exe') -ArgumentList @('--no-ui', '--plugins-dir', $pluginsDir) -WindowStyle Hidden -PassThru
Start-Sleep -Seconds 3
$pipe2 = New-Object System.IO.Pipes.NamedPipeClientStream('.', 'spark.host.ipc', [System.IO.Pipes.PipeDirection]::InOut)
$pipe2.Connect(5000)
$writer2 = New-Object System.IO.StreamWriter($pipe2, $enc); $writer2.AutoFlush = $true; $writer2.NewLine = "`n"
$reader2 = New-Object System.IO.StreamReader($pipe2, $enc)
$script:writer = $writer2
$script:reader = $reader2
$r5 = Invoke-Rpc 'host.plugin.list' @{}
$off2 = $r5.result | Where-Object { $_.id -eq 'com.spark.sign.official' }
if ($off2.sign_state -ne 'invalid') { $failures.Add('startup reverify expected invalid, got ' + $off2.sign_state) }
$writer2.Dispose(); $reader2.Dispose(); $pipe2.Dispose()
$proc2.Kill(); $proc2.WaitForExit(5000) | Out-Null

if ($failures.Count -gt 0) {
  Write-Output 'E2E FAILED:'
  foreach ($f in $failures) { Write-Output ('  - ' + $f) }
  exit 1
}
Write-Output 'E2E PASSED: official=official tampered=rejected unsigned=unsigned startup-reverify=invalid'