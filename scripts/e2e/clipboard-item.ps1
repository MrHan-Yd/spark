# e2e: spark.clipboard.read / write (multi-format, host pipe, ASCII only)
#
# Covers: host.plugin.api capability=clipboard
#   write_item {text+imagePng} -> read_all returns both formats, pixels survive the
#   PNG -> WIC decode -> CF_DIB write -> CF_DIBV5 read -> PNG round trip;
#   write_item empty -> INVALID_ARGS; write_item bad base64 -> UNSUPPORTED_FORMAT;
#   read_all after text-only write -> types == ['text/plain'].
#
# NOTE: this script drives the REAL system clipboard through the host process
# (CF_UNICODETEXT + CF_DIB), so it OVERWRITES whatever the user had copied.
# Run it only when that is acceptable.
#
# It also requires a session that can actually open the Windows clipboard: a headless /
# service-session shell fails with CLIPBRD_E_CANT_OPEN (0x800401D0) before any Spark code
# runs (the sibling clipboard-image.ps1 fails the same way), which would look like a
# product bug. If you see that error, run from a normal interactive desktop session.
#
# Prereq: cargo build -p spark-host (cargo test does not relink the bin), and
#         taskkill /IM spark-host.exe /F (a running host steals the pipe).

$ErrorActionPreference = 'Stop'
# Repo root = two levels above this script (scripts/e2e/*.ps1).
$root = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$hostExeSrc = Join-Path $root 'target\debug\spark-host.exe'

$tmp = Join-Path $env:TEMP ('spark_clipitem_e2e_' + [guid]::NewGuid().ToString('N'))
$hostDir = Join-Path $tmp 'host'
$pluginsDir = Join-Path $tmp 'plugins'
$devPlugin = Join-Path $tmp 'devplugin'
New-Item -ItemType Directory -Path (Join-Path $hostDir 'data'), $pluginsDir, $devPlugin -Force | Out-Null
Copy-Item $hostExeSrc (Join-Path $hostDir 'spark-host.exe')

$enc = New-Object System.Text.UTF8Encoding($false)
$manifest = '{"id":"com.spark.clip.e2e","name":"ClipE2E","version":"0.1.0","api_version":2,"runtime":"webview","main":"index.html","permissions":["clipboard"],"features":[{"type":"keyword","keyword":"clipe2e","title":"Clip","mode":"page"}]}'
[System.IO.File]::WriteAllText((Join-Path $devPlugin 'plugin.json'), $manifest, $enc)
[System.IO.File]::WriteAllText((Join-Path $devPlugin 'index.html'), '<html></html>', $enc)

$proc = Start-Process -FilePath (Join-Path $hostDir 'spark-host.exe') -ArgumentList @('--no-ui', '--plugins-dir', $pluginsDir) -WindowStyle Hidden -PassThru
Start-Sleep -Seconds 3

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
function Clip([string]$m, $arg) {
  return Invoke-Rpc 'host.plugin.api' @{ plugin_id = 'com.spark.clip.e2e'; capability = 'clipboard'; method = $m; args = $arg }
}

$failures = New-Object System.Collections.Generic.List[string]

$dl = Invoke-Rpc 'host.plugin.devload' @{ dir = $devPlugin }
if (-not $dl.result.id) { $failures.Add('devload failed') }
$gr = Invoke-Rpc 'host.plugin.grant' @{ id = 'com.spark.clip.e2e'; permissions = @('clipboard') }
if (-not $gr.result.ok) { $failures.Add('grant failed') }

# ---- 2x2 PNG (red / green / blue / white) built with System.Drawing ----
Add-Type -AssemblyName System.Drawing
$bmp = New-Object System.Drawing.Bitmap(2, 2)
$bmp.SetPixel(0, 0, [System.Drawing.Color]::FromArgb(255, 255, 0, 0))
$bmp.SetPixel(1, 0, [System.Drawing.Color]::FromArgb(255, 0, 128, 0))
$bmp.SetPixel(0, 1, [System.Drawing.Color]::FromArgb(255, 0, 0, 255))
$bmp.SetPixel(1, 1, [System.Drawing.Color]::FromArgb(255, 255, 255, 255))
$ms = New-Object System.IO.MemoryStream
$bmp.Save($ms, [System.Drawing.Imaging.ImageFormat]::Png)
$pngB64 = [Convert]::ToBase64String($ms.ToArray())
$bmp.Dispose(); $ms.Dispose()

# ---- 1) write text + image in one call, then read both back ----
$w = Clip 'write_item' @{ text = 'spark-clip-e2e'; image_png = $pngB64 }
if (-not $w.result.data.ok) { $failures.Add('write_item(text+image) failed: ' + ($w | ConvertTo-Json -Compress -Depth 6)) }

$r = Clip 'read_all' @{}
if ($r.error) { $failures.Add('read_all errored: ' + $r.error.message) }
else {
  $types = @($r.result.data.types)
  if ($types -notcontains 'text/plain') { $failures.Add('read_all types missing text/plain: ' + ($types -join ',')) }
  if ($types -notcontains 'image/png') { $failures.Add('read_all types missing image/png: ' + ($types -join ',')) }
  if ($r.result.data.text -ne 'spark-clip-e2e') { $failures.Add('read_all text=' + $r.result.data.text) }

  # decode the returned PNG and check the 4 pixels survive the whole round trip
  $outB64 = $r.result.data.image_png
  if (-not $outB64) { $failures.Add('read_all image_png empty') }
  else {
    $bytes = [Convert]::FromBase64String($outB64)
    $ims = New-Object System.IO.MemoryStream(, $bytes)
    $outBmp = New-Object System.Drawing.Bitmap($ims)
    if ($outBmp.Width -ne 2 -or $outBmp.Height -ne 2) {
      $failures.Add('round trip size=' + $outBmp.Width + 'x' + $outBmp.Height)
    }
    else {
      $p00 = $outBmp.GetPixel(0, 0); $p10 = $outBmp.GetPixel(1, 0)
      $p01 = $outBmp.GetPixel(0, 1); $p11 = $outBmp.GetPixel(1, 1)
      if ($p00.R -ne 255 -or $p00.G -ne 0 -or $p00.B -ne 0) { $failures.Add('pixel(0,0)=' + $p00.ToString()) }
      if ($p10.R -ne 0 -or $p10.G -ne 128 -or $p10.B -ne 0) { $failures.Add('pixel(1,0)=' + $p10.ToString()) }
      if ($p01.R -ne 0 -or $p01.G -ne 0 -or $p01.B -ne 255) { $failures.Add('pixel(0,1)=' + $p01.ToString()) }
      if ($p11.R -ne 255 -or $p11.G -ne 255 -or $p11.B -ne 255) { $failures.Add('pixel(1,1)=' + $p11.ToString()) }
    }
    $outBmp.Dispose(); $ims.Dispose()
  }
}

# ---- 2) text-only write replaces the clipboard: image format must disappear ----
$w2 = Clip 'write_item' @{ text = 'text-only' }
if (-not $w2.result.data.ok) { $failures.Add('write_item(text only) failed') }
$r2 = Clip 'read_all' @{}
$types2 = @($r2.result.data.types)
if ($types2 -notcontains 'text/plain') { $failures.Add('text-only read_all missing text/plain') }
if ($types2 -contains 'image/png') { $failures.Add('text-only write left stale image/png (EmptyClipboard not honored)') }

# ---- 3) validation errors ----
$e1 = Clip 'write_item' @{}
if (-not $e1.error -or $e1.error.message -notmatch 'INVALID_ARGS') { $failures.Add('empty write not rejected: ' + ($e1 | ConvertTo-Json -Compress -Depth 6)) }

$e2 = Clip 'write_item' @{ image_png = '!!!not base64!!!' }
if (-not $e2.error -or $e2.error.message -notmatch 'INVALID_ARGS') { $failures.Add('bad base64 not rejected: ' + ($e2 | ConvertTo-Json -Compress -Depth 6)) }

$e3 = Clip 'write_item' @{ image_png = ([Convert]::ToBase64String([Text.Encoding]::ASCII.GetBytes('not a png at all'))) }
if (-not $e3.error -or $e3.error.message -notmatch 'UNSUPPORTED_FORMAT') { $failures.Add('non-PNG not rejected as UNSUPPORTED_FORMAT: ' + ($e3 | ConvertTo-Json -Compress -Depth 6)) }

$e4 = Clip 'write_item' @{ image_png = 12345 }
if (-not $e4.error -or $e4.error.message -notmatch 'INVALID_ARGS') { $failures.Add('non-string imagePng not rejected') }

# ---- 4) failed validation must NOT have clobbered the clipboard ----
$r4 = Clip 'read_text' @{}
if ($r4.result.data.text -ne 'text-only') { $failures.Add('clipboard clobbered by rejected write: ' + $r4.result.data.text) }

# ---- 5) legacy single-format methods still work ----
$w5 = Clip 'write_text' @{ text = 'legacy-path' }
if (-not $w5.result.data.ok) { $failures.Add('write_text failed') }
$r5 = Clip 'read_text' @{}
if ($r5.result.data.text -ne 'legacy-path') { $failures.Add('read_text=' + $r5.result.data.text) }

# ---- 6) host still answers on other methods (no long lock held by clipboard IO) ----
$lst = Invoke-Rpc 'host.plugin.list' @{}
if (-not ($lst.result | Where-Object { $_.id -eq 'com.spark.clip.e2e' })) { $failures.Add('host unresponsive after clipboard ops') }

$writer.Dispose(); $reader.Dispose(); $pipe.Dispose()
$proc.Kill(); $proc.WaitForExit(5000) | Out-Null

if ($failures.Count -gt 0) {
  Write-Output 'E2E FAILED:'
  foreach ($f in $failures) { Write-Output ('  - ' + $f) }
  exit 1
}
Write-Output 'E2E PASSED: clipboard write_item/read_all round trip (text + 2x2 PNG pixels) / text-only replaces image / INVALID_ARGS + UNSUPPORTED_FORMAT / rejected write keeps clipboard / legacy methods'
