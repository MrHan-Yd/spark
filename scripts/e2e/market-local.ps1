# e2e: local plugin-market warehouse fixture + fetch reachability check (ASCII only)
#
# Builds a self-consistent local registry fixture under %TEMP%\spark-market-fixture\site:
#   registry.json          - schema=1 index with 4 entries (tags / no-tags / bad tags / incomplete)
#   hello-0.1.0.zip        - webview plugin packed from plugins\hello  (direct url + sha256)
#   echo-0.2.0.zip         - native plugin packed from plugins\echo
#   bad\schema2.json       - schema=2 (unsupported version)
#   bad\notjson.json       - not valid JSON
#   bad\bad-tags.json      - tags over the documented limits (12 chars / 4 per plugin)
#                            + non-string items (number / null / nested object / array)
#
# Then serves it over HTTP and asserts: index reachable + parses, schema=1, every
# `url`-based version downloads and its sha256 matches the index value, and each bad
# fixture is malformed in exactly the way it claims.
#
# What this does NOT cover: RegistryService (index parsing tolerance, tag sanitizing and
# the install pipeline) lives in the C# UI process, which has no test project in this repo.
# The fixture exists so that part can be exercised by hand in the UI - the script prints
# the exact steps at the end.
#
# Prereq: python on PATH (for the throwaway HTTP server).

param([int]$Port = 8731, [string]$PythonExe = '')

$ErrorActionPreference = 'Stop'
# Repo root = two levels above this script (scripts/e2e/*.ps1), never hard-coded.
$root = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)

$fixtureRoot = Join-Path $env:TEMP 'spark-market-fixture'
# Guard: only ever touch the fixture directory this script owns.
if (-not $fixtureRoot.EndsWith('spark-market-fixture')) { throw 'refusing to clean unexpected path' }
if (Test-Path $fixtureRoot) { Remove-Item $fixtureRoot -Recurse -Force }
$site = Join-Path $fixtureRoot 'site'
New-Item -ItemType Directory -Path $site, (Join-Path $site 'bad') -Force | Out-Null

$enc = New-Object System.Text.UTF8Encoding($false)
function Write-Utf8NoBom([string]$path, [string]$text) {
  [System.IO.File]::WriteAllText($path, $text, $enc)
}

function New-PluginZip([string]$srcDir, [string]$outZip) {
  if (-not (Test-Path $srcDir)) { throw "missing source dir: $srcDir" }
  if (Test-Path $outZip) { Remove-Item $outZip -Force }
  Compress-Archive -Path (Join-Path $srcDir '*') -DestinationPath $outZip -CompressionLevel Optimal
  return (Get-FileHash $outZip -Algorithm SHA256).Hash.ToLowerInvariant()
}

$failures = New-Object System.Collections.Generic.List[string]

# ---- 1) pack the two sample plugins (sha256 computed from the real bytes) ----
$helloSha = New-PluginZip (Join-Path $root 'plugins\hello') (Join-Path $site 'hello-0.1.0.zip')
$echoSha = New-PluginZip (Join-Path $root 'plugins\echo') (Join-Path $site 'echo-0.2.0.zip')

# ---- 2) registry.json (schema=1) ----
# Tags stay ASCII here except one entry using JSON \u escapes, which keeps the script
# ASCII-only while still exercising a non-ASCII tag end to end.
$registry = @'
{
  "schema": 1,
  "name": "Spark Local Fixture",
  "zipball_url": "http://127.0.0.1:__PORT__/nonexistent.zip",
  "updated": "2026-09-10T00:00:00Z",
  "plugins": [
    {
      "id": "com.spark.hello",
      "name": "Hello (local)",
      "description": "webview sample, direct zip url",
      "author": "fixture",
      "homepage": null,
      "icon": null,
      "runtime": "webview",
      "permissions": ["clipboard", "notify"],
      "tags": ["Tools", "Text"],
      "latest": "0.1.0",
      "versions": [
        { "version": "0.1.0", "path": null, "url": "http://127.0.0.1:__PORT__/hello-0.1.0.zip", "sha256": "__HELLOSHA__", "size": null, "released": "2026-09-10" }
      ]
    },
    {
      "id": "com.spark.echo",
      "name": "Echo (local)",
      "description": "native sample, direct zip url",
      "author": "fixture",
      "homepage": null,
      "icon": null,
      "runtime": "native",
      "permissions": [],
      "tags": ["Tools", "\u6f14\u793a"],
      "latest": "0.2.0",
      "versions": [
        { "version": "0.2.0", "path": null, "url": "http://127.0.0.1:__PORT__/echo-0.2.0.zip", "sha256": "__ECHOSHA__", "size": null, "released": "2026-09-10" }
      ]
    },
    {
      "id": "com.spark.notags",
      "name": "No Tags",
      "description": "no tags field at all -> UI bucket 'untagged'",
      "author": "fixture",
      "runtime": "webview",
      "permissions": [],
      "latest": "0.1.0",
      "versions": [ { "version": "0.1.0", "path": null, "url": "http://127.0.0.1:__PORT__/hello-0.1.0.zip", "sha256": "__HELLOSHA__" } ]
    },
    {
      "id": "com.spark.incomplete",
      "name": "Incomplete Entry",
      "description": "missing 'latest' -> UI must skip this entry without failing the whole index",
      "author": "fixture",
      "runtime": "webview",
      "permissions": [],
      "tags": ["Broken"],
      "versions": []
    }
  ]
}
'@

$registry = $registry.Replace('__PORT__', $Port.ToString())
$registry = $registry.Replace('__HELLOSHA__', $helloSha)
$registry = $registry.Replace('__ECHOSHA__', $echoSha)
Write-Utf8NoBom (Join-Path $site 'registry.json') $registry

# ---- 3) malformed indexes (each one targets a documented tolerance rule) ----
Write-Utf8NoBom (Join-Path $site 'bad\schema2.json') '{ "schema": 2, "name": "future", "plugins": [] }'
Write-Utf8NoBom (Join-Path $site 'bad\notjson.json') '{ "schema": 1, "plugins": [ this is not json'
$badTags = @'
{
  "schema": 1,
  "name": "bad tags",
  "plugins": [
    {
      "id": "com.spark.badtags", "name": "Bad Tags", "runtime": "webview",
      "latest": "0.1.0",
      "tags": ["ok", "duplicate", "DUPLICATE", "this-tag-is-way-too-long", "fifth", "sixth", "with\u0007control"],
      "versions": [ { "version": "0.1.0", "path": "x", "url": null } ]
    },
    {
      "id": "com.spark.badtypes", "name": "Bad Tag Types", "runtime": "webview",
      "latest": "0.1.0",
      "tags": [123, null, "", "  ", "fine", {"nested": "object"}, ["nested", "array"]],
      "versions": [ { "version": "0.1.0", "path": "x", "url": null } ]
    }
  ]
}
'@
Write-Utf8NoBom (Join-Path $site 'bad\bad-tags.json') $badTags

# ---- 4) fixture self-check (no server yet): the files must be what the header claims ----
$idx = Get-Content (Join-Path $site 'registry.json') -Raw -Encoding UTF8 | ConvertFrom-Json
# non-ASCII tag used by the fixture, kept as code points so this script stays ASCII-only
$zhDemo = [string][char]0x6F14 + [string][char]0x793A
if ($idx.schema -ne 1) { $failures.Add('fixture: schema != 1') }
if (@($idx.plugins).Count -ne 4) { $failures.Add('fixture: expected 4 plugin entries') }
if (-not ($idx.plugins | Where-Object { $_.id -eq 'com.spark.hello' -and $_.tags.Count -eq 2 })) { $failures.Add('fixture: hello tags') }
if (($idx.plugins | Where-Object { $_.id -eq 'com.spark.echo' }).tags[1] -ne $zhDemo) { $failures.Add('fixture: echo non-ascii tag not decoded') }
if (($idx.plugins | Where-Object { $_.id -eq 'com.spark.incomplete' }).latest) { $failures.Add('fixture: incomplete entry should have no latest') }

try {
  Get-Content (Join-Path $site 'bad\notjson.json') -Raw -Encoding UTF8 | ConvertFrom-Json | Out-Null
  $failures.Add('fixture: notjson.json unexpectedly parsed')
} catch { }

$bt = Get-Content (Join-Path $site 'bad\bad-tags.json') -Raw -Encoding UTF8 | ConvertFrom-Json
if (@($bt.plugins[0].tags).Count -lt 5) { $failures.Add('fixture: bad-tags should exceed the 4-tag cap') }
if (-not (@($bt.plugins[0].tags) | Where-Object { $_.Length -gt 12 })) { $failures.Add('fixture: bad-tags should exceed the 12-char cap') }
if (-not (@($bt.plugins[1].tags) | Where-Object { $_ -isnot [string] })) { $failures.Add('fixture: badtypes should contain non-string/nested tag items') }

# ---- 5) serve + reachability ----
# python is only used as a throwaway static file server. Resolve it from PATH (`python`
# then the `py` launcher); -PythonExe is an escape hatch for shells where PATH lookup is
# restricted (sandboxed/CI shells often cannot resolve arbitrary executables).
$pyArgs = @('-m', 'http.server', $Port.ToString(), '--bind', '127.0.0.1', '--directory', $site)
if ($PythonExe) {
  if (-not (Test-Path $PythonExe)) { throw "python not found at $PythonExe" }
  $pyExe = @{ Source = $PythonExe }
} else {
  $pyExe = (Get-Command python -ErrorAction SilentlyContinue)
  if (-not $pyExe) {
    $pyExe = (Get-Command py -ErrorAction SilentlyContinue)
    if ($pyExe) { $pyArgs = @('-3') + $pyArgs }
  }
  if (-not $pyExe) {
    throw 'python/py not found on PATH (needed for the throwaway HTTP server; pass -PythonExe <path>)'
  }
}

$server = Start-Process -FilePath $pyExe.Source -ArgumentList $pyArgs -WindowStyle Hidden -PassThru
try {
  $ready = $false
  for ($i = 0; $i -lt 20 -and -not $ready; $i++) {
    Start-Sleep -Milliseconds 250
    try { Invoke-WebRequest "http://127.0.0.1:$Port/registry.json" -UseBasicParsing -TimeoutSec 3 | Out-Null; $ready = $true } catch { }
  }
  if (-not $ready) { throw "local server on port $Port never became ready" }

  $resp = Invoke-WebRequest "http://127.0.0.1:$Port/registry.json" -UseBasicParsing -TimeoutSec 10
  if ($resp.StatusCode -ne 200) { $failures.Add('GET registry.json status=' + $resp.StatusCode) }
  $served = $resp.Content | ConvertFrom-Json
  if ($served.schema -ne 1) { $failures.Add('served index schema != 1') }

  # every url-based version must download, and sha256 must match the index value
  foreach ($p in $served.plugins) {
    foreach ($v in @($p.versions)) {
      if (-not $v.url) { continue }
      $zipPath = Join-Path $fixtureRoot ('dl_' + $p.id + '.zip')
      Invoke-WebRequest $v.url -UseBasicParsing -TimeoutSec 20 -OutFile $zipPath
      $got = (Get-FileHash $zipPath -Algorithm SHA256).Hash.ToLowerInvariant()
      if ($got -ne $v.sha256) { $failures.Add("sha256 mismatch for $($p.id): index=$($v.sha256) file=$got") }
      # zip must carry plugin.json at root or in a single top-level dir (ExtractZipSafely contract)
      Add-Type -AssemblyName System.IO.Compression.FileSystem
      $zip = [System.IO.Compression.ZipFile]::OpenRead($zipPath)
      $names = @($zip.Entries | ForEach-Object { $_.FullName })
      $zip.Dispose()
      if (-not ($names -contains 'plugin.json')) { $failures.Add("$($p.id) zip has no root plugin.json: " + ($names -join ',')) }
    }
  }

  # malformed fixtures must be served but stay malformed
  $s2 = Invoke-WebRequest "http://127.0.0.1:$Port/bad/schema2.json" -UseBasicParsing -TimeoutSec 10
  if (($s2.Content | ConvertFrom-Json).schema -ne 2) { $failures.Add('bad/schema2.json not served as schema=2') }
  $nj = Invoke-WebRequest "http://127.0.0.1:$Port/bad/notjson.json" -UseBasicParsing -TimeoutSec 10
  try { $nj.Content | ConvertFrom-Json | Out-Null; $failures.Add('served notjson.json unexpectedly parsed') } catch { }
}
finally {
  if ($server -and -not $server.HasExited) { $server.Kill(); $server.WaitForExit(5000) | Out-Null }
}

if ($failures.Count -gt 0) {
  Write-Output 'E2E FAILED:'
  foreach ($f in $failures) { Write-Output ('  - ' + $f) }
  exit 1
}

Write-Output 'E2E PASSED: local registry fixture built, served, index/zip/sha256 reachable, malformed variants stay malformed'
Write-Output ''
Write-Output 'Manual UI round (index parsing + tags grouping + install live in the C# UI process, no test project in repo):'
Write-Output ('  1) python -m http.server ' + $Port + ' --bind 127.0.0.1 --directory "' + $site + '"')
Write-Output ('  2) Spark -> Settings -> Plugins -> Marketplace -> Manage repositories -> add http://127.0.0.1:' + $Port + '/registry.json -> Save')
Write-Output ('  3) expect: 4 entries, one skipped (missing latest); tag bar shows Tools/Text/Broken/' + $zhDemo + '; No Tags sits under the untagged group; grouping headers appear')
Write-Output '  4) install "Hello (local)" -> marketplace AND installed list refresh; "No Tags" installs from the same direct url'
Write-Output ('  5) negative: point a repo at http://127.0.0.1:' + $Port + '/bad/notjson.json (index format error, no crash), /bad/schema2.json (unsupported version), /bad/bad-tags.json (tags sanitized, entries still listed)')
