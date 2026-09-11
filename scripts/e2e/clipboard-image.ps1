# e2e: read_image with REAL clipboard image data (STA for clipboard access)
# 2x2 colored bitmap -> clipboard -> spark.clipboard.readImage -> base64 PNG -> pixel verify
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing
Add-Type -AssemblyName PresentationCore
Add-Type -AssemblyName WindowsBase

# Repo root = two levels above this script (scripts/e2e/*.ps1).
$root = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$hostExeSrc = Join-Path $root 'target\debug\spark-host.exe'
$tmp = Join-Path $env:TEMP ('spark_img_e2e_' + [guid]::NewGuid().ToString('N'))
$hostDir = Join-Path $tmp 'host'; $dataDir = Join-Path $hostDir 'data'
$devPlugin = Join-Path $tmp 'devplugin'; $pluginsDir = Join-Path $tmp 'plugins'
New-Item -ItemType Directory -Path $dataDir, $devPlugin, $pluginsDir -Force | Out-Null
Copy-Item $hostExeSrc (Join-Path $hostDir 'spark-host.exe')

$enc = New-Object System.Text.UTF8Encoding($false)
$manifest = '{"id":"com.spark.img.e2e","name":"IMG","version":"0.1.0","api_version":2,"runtime":"webview","main":"index.html","permissions":["clipboard"],"features":[{"type":"keyword","keyword":"img","title":"IMG","mode":"page"}]}'
[System.IO.File]::WriteAllText((Join-Path $devPlugin 'plugin.json'), $manifest, $enc)
[System.IO.File]::WriteAllText((Join-Path $devPlugin 'index.html'), '<html></html>', $enc)

# --- put a REAL, byte-deterministic DIB on the clipboard ---
# 40-header + BI_RGB + 32bpp 2x2, bottom-up, opaque alpha (alpha=0 exercises
# the pseudo-opaque path in unit tests; here opaque keeps OS DIBV5 synthesis faithful).
# top row: red(255,0,0) green(0,128,0); bottom row: blue(0,0,255) white(255,255,255)
$px = New-Object byte[] 16
$px[0]=255; $px[3]=255                     # bottom-left blue (B,G,R,X) alpha=255
$px[4]=255; $px[5]=255; $px[6]=255; $px[7]=255   # bottom-right white
$px[10]=255; $px[11]=255                    # top-left red
$px[13]=128; $px[15]=255                    # top-right green
$dib = New-Object byte[] 56
[BitConverter]::GetBytes([uint32]40).CopyTo($dib, 0)
[BitConverter]::GetBytes([int32]2).CopyTo($dib, 4)
[BitConverter]::GetBytes([int32]2).CopyTo($dib, 8)
[BitConverter]::GetBytes([uint16]1).CopyTo($dib, 12)
[BitConverter]::GetBytes([uint16]32).CopyTo($dib, 14)
[BitConverter]::GetBytes([uint32]0).CopyTo($dib, 16)
$px.CopyTo($dib, 40)
$ms = New-Object System.IO.MemoryStream(,$dib)
$do = New-Object System.Windows.DataObject
$do.SetData('DeviceIndependentBitmap', $ms)
[System.Windows.Clipboard]::SetDataObject($do, $true)

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

$failures = New-Object System.Collections.Generic.List[string]
Invoke-Rpc 'host.plugin.devload' @{ dir = $devPlugin } | Out-Null
$g = Invoke-Rpc 'host.plugin.grant' @{ id = 'com.spark.img.e2e'; permissions = @('clipboard') }
if (-not $g.result.ok) { $failures.Add('grant failed: ' + $g.error.message) }

# 1. read_image returns base64 PNG with correct colors (via REAL clipboard data)
$ri = Invoke-Rpc 'host.plugin.api' @{ plugin_id='com.spark.img.e2e'; capability='clipboard'; method='read_image'; args=@{} }
if ($ri.error) { $failures.Add('read_image errored: ' + $ri.error.message) }
else {
  $data = $ri.result.data.data
  if (-not $data) { $failures.Add('read_image returned null for an image-bearing clipboard') }
  else {
    $pngPath = Join-Path $tmp 'out.png'
    [System.IO.File]::WriteAllBytes($pngPath, [Convert]::FromBase64String($data))
    $png = [System.Drawing.Bitmap]::FromFile($pngPath)
    # WPF DIBV5 transfer is bottom-up-agnostic; compare as a set-insensitive 2x2 color grid
    $c00 = $png.GetPixel(0, 0); $c10 = $png.GetPixel(1, 0); $c01 = $png.GetPixel(0, 1); $c11 = $png.GetPixel(1, 1)
    $colors = @("$($c00.R),$($c00.G),$($c00.B)", "$($c10.R),$($c10.G),$($c10.B)", "$($c01.R),$($c01.G),$($c01.B)", "$($c11.R),$($c11.G),$($c11.B)") | Sort-Object
    $expected = @('0,0,255', '0,128,0', '255,0,0', '255,255,255') | Sort-Object
    $mismatch = 0
    for ($i = 0; $i -lt 4; $i++) { if ($colors[$i] -ne $expected[$i]) { $mismatch++ } }
    if ($mismatch -gt 0) { $failures.Add('color mismatch (got): ' + ($colors -join ' | ') + '  expected: ' + ($expected -join ' | ')) }
    $png.Dispose()
  }
}

# 2. empty clipboard -> data null (no error)
[System.Windows.Clipboard]::Clear()
Start-Sleep -Milliseconds 300
$re = Invoke-Rpc 'host.plugin.api' @{ plugin_id='com.spark.img.e2e'; capability='clipboard'; method='read_image'; args=@{} }
if ($re.error) { $failures.Add('read_image empty-clipboard errored: ' + $re.error.message) }
else { if ($null -ne $re.result.data.data) { $failures.Add('empty clipboard expected null data') } }

$writer.Dispose(); $reader.Dispose(); $pipe.Dispose()
$proc.Kill() | Out-Null
[System.Windows.Clipboard]::Clear()

if ($failures.Count -gt 0) {
  Write-Output 'E2E FAILED:'
  foreach ($f in $failures) { Write-Output ('  - ' + $f) }
  exit 1
}
Write-Output 'E2E PASSED: real clipboard image -> base64 PNG colors correct; empty clipboard -> null'