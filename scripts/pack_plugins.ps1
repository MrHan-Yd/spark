# pack_plugins.ps1 - Spark official plugin packaging + warehouse index generation.
#
# Turns plugins\<id>\ sources into a warehouse-ready output tree:
#
#   <OutDir>/
#     registry.json                       index (schema=1), ready to publish
#     <id>/<version>/                     version dir = the unit CI signs and the
#                                         target of versions[].path in path mode
#     packages/<id>-<version>.spark-plugin  packed zip (direct-download / manual install)
#
# Pipeline order matters and is enforced here:
#   build native exe -> stage version dir -> (sign) -> pack zip -> write registry.json
# Signing MUST happen before packing, otherwise the zip ships without signature.json.
#
# Warehouse fields that plugin.json does not carry (tags / homepage / icon) come from an
# optional sidecar plugins\<id>\market.json - tags belong to the index, not to the plugin
# manifest. Tag limits are validated here so a bad tag fails the pack instead of being
# silently dropped by the client at install time (see RegistryService.NormalizeTags).
#
# ASCII-only on purpose: PowerShell 5.1 reads BOM-less UTF-8 scripts as ANSI, so non-ASCII
# literals here would garble. Chinese content comes from the plugin files, not this script.
#
# Examples:
#   ./scripts/pack_plugins.ps1                        # build native exe, stage, pack, index (path mode)
#   ./scripts/pack_plugins.ps1 -SkipBuild             # reuse target/<cfg> artifacts
#   ./scripts/pack_plugins.ps1 -Sign -KeyFile keys/spark-official-v1.key
#   ./scripts/pack_plugins.ps1 -BaseUrl https://example.com/spark-plugins -ZipballUrl https://example.com/spark-plugins/archive/master.zip

[CmdletBinding()]
param(
  # Output root. Default matches the release.yml signing step contract (plugins/dist).
  [string]$OutDir = 'plugins/dist',
  # cargo profile for native plugin exes: release | debug.
  [ValidateSet('release', 'debug')]
  [string]$Configuration = 'release',
  # Reuse existing target\<cfg> artifacts instead of rebuilding native plugins.
  [switch]$SkipBuild,
  # Wipe the whole output dir before packing (destructive; guarded to stay inside the repo).
  [switch]$Clean,
  # Sign each staged version dir with spark-sign before packing.
  [switch]$Sign,
  # Private key file for -Sign (base64 seed).
  [string]$KeyFile = 'keys/spark-official-v1.key',
  [string]$KeyId = 'spark-official-v1',
  # Path prefix written into versions[].path (path mode). Defaults to <OutDir> relative to
  # the repo root, so `spark-sign check-registry --dir <repo root>` resolves the same dirs.
  [string]$PathPrefix = '',
  # Direct-download mode: when set, versions[].url = <BaseUrl>/packages/<file> and the
  # packed zip's sha256/size are recorded. Without it, path mode is used (url/sha256 null).
  [string]$BaseUrl = '',
  # Warehouse zipball URL recorded in the index (path mode needs it; official sources can
  # rely on the built-in fallback, third-party ones cannot).
  [string]$ZipballUrl = '',
  # Warehouse display name written to registry.json.
  [string]$RegistryName = 'Spark Official Plugins'
)

$ErrorActionPreference = 'Stop'
$repoRoot = Split-Path $PSScriptRoot -Parent

# cargo / spark-sign print UTF-8; without this, PowerShell decodes their stdout using the
# ANSI code page and the audit artifact (signing-log.txt) ends up mojibake.
try { [Console]::OutputEncoding = New-Object System.Text.UTF8Encoding($false) } catch { }

# ---------------------------------------------------------------- helpers

function Write-Utf8NoBom([string]$path, [string]$text) {
  # No BOM: both serde_json and System.Text.Json reject a leading BOM, so a BOM here would
  # make the whole warehouse unreadable from the client.
  [System.IO.File]::WriteAllText($path, $text, (New-Object System.Text.UTF8Encoding($false)))
}

function Get-ExeSuffix { if ($env:OS -eq 'Windows_NT') { '.exe' } else { '' } }

function Get-RelativePathCompat([string]$basePath, [string]$targetPath) {
  # Path.GetRelativePath is .NET Core only; PowerShell 5.1 (.NET Framework) lacks it, so
  # fall back to a URI-based computation (both paths are known to be under the same root).
  if ([System.IO.Path].GetMethod('GetRelativePath', [type[]]@([string], [string]))) {
    return [System.IO.Path]::GetRelativePath($basePath, $targetPath)
  }
  $baseUri = New-Object System.Uri (([System.IO.Path]::GetFullPath($basePath).TrimEnd('\') + '\'))
  $targetUri = New-Object System.Uri ([System.IO.Path]::GetFullPath($targetPath))
  return [System.Uri]::UnescapeDataString($baseUri.MakeRelativeUri($targetUri).ToString()).Replace('/', '\')
}

function ConvertTo-JsonString([string]$value) {
  # Minimal JSON string writer: ConvertTo-Json escapes non-ASCII to \uXXXX on PS 5.1 but not
  # on PS 7, which would make registry.json diffs depend on the shell. We want stable,
  # human-reviewable UTF-8, so build the string ourselves.
  if ($null -eq $value) { return 'null' }
  $sb = New-Object System.Text.StringBuilder
  [void]$sb.Append('"')
  foreach ($ch in $value.ToCharArray()) {
    $code = [int]$ch
    if ($ch -eq '"') { [void]$sb.Append('\"') }
    elseif ($ch -eq '\') { [void]$sb.Append('\\') }
    elseif ($code -eq 8) { [void]$sb.Append('\b') }
    elseif ($code -eq 12) { [void]$sb.Append('\f') }
    elseif ($code -eq 10) { [void]$sb.Append('\n') }
    elseif ($code -eq 13) { [void]$sb.Append('\r') }
    elseif ($code -eq 9) { [void]$sb.Append('\t') }
    elseif ($code -lt 0x20) { [void]$sb.Append('\u' + ('{0:x4}' -f $code)) }
    else { [void]$sb.Append($ch) }
  }
  [void]$sb.Append('"')
  return $sb.ToString()
}

function Format-JsonScalar($value) {
  if ($null -eq $value) { return 'null' }
  if ($value -is [bool]) { if ($value) { return 'true' } else { return 'false' } }
  if ($value -is [int] -or $value -is [long] -or $value -is [double]) { return ([string]$value) }
  return (ConvertTo-JsonString ([string]$value))
}

# Same limits as RegistryService.NormalizeTags on the client: a tag that would be dropped at
# runtime must fail here instead, otherwise the index ships tags that silently disappear.
$MaxTagLength = 12
$MaxTagsPerPlugin = 4

function Assert-Tags([string]$pluginId, $rawTags) {
  if ($null -eq $rawTags) { return @() }
  if ($rawTags -isnot [System.Collections.IEnumerable] -or $rawTags -is [string]) {
    throw "market.json: 'tags' must be an array (plugin $pluginId)"
  }
  $tags = @()
  $seen = New-Object 'System.Collections.Generic.HashSet[string]' ([System.StringComparer]::OrdinalIgnoreCase)
  foreach ($t in $rawTags) {
    $tag = [string]$t
    $tag = $tag.Trim()
    if ($tag.Length -eq 0) { throw "market.json: empty tag (plugin $pluginId)" }
    if ($tag.Length -gt $MaxTagLength) { throw "market.json: tag '$tag' exceeds $MaxTagLength chars (plugin $pluginId)" }
    if ($tag.ToCharArray() | Where-Object { [char]::IsControl($_) }) { throw "market.json: tag '$tag' contains control chars (plugin $pluginId)" }
    if ($tags.Count -ge $MaxTagsPerPlugin) { throw "market.json: more than $MaxTagsPerPlugin tags (plugin $pluginId)" }
    if ($seen.Add($tag)) { $tags += $tag }
  }
  return $tags
}

function Format-VersionEntry {
  param([string]$Version, $PathValue, $Url, $Sha, $Size, [string]$Released, $Signature)
  $lines = @()
  $lines += '        {'
  $lines += '          "version": ' + (ConvertTo-JsonString $Version) + ','
  $lines += '          "path": ' + (Format-JsonScalar $PathValue) + ','
  $lines += '          "url": ' + (Format-JsonScalar $Url) + ','
  $lines += '          "sha256": ' + (Format-JsonScalar $Sha) + ','
  $lines += '          "size": ' + (Format-JsonScalar $Size) + ','
  $lines += '          "released": ' + (ConvertTo-JsonString $Released) + $(if ($Signature) { ',' } else { '' })
  if ($Signature) {
    # Signature summary in the index (spec §3.3): the download-free "official" badge
    # pre-judgment. Only schema/key_id/algorithm/signature - the file list stays in the
    # package (the package copy is authoritative, the index value is display-only).
    $lines += '          "signature": { "schema": ' + (Format-JsonScalar ([int]$Signature.schema)) + ', "key_id": ' + (ConvertTo-JsonString ([string]$Signature.key_id)) + ', "algorithm": ' + (ConvertTo-JsonString ([string]$Signature.algorithm)) + ', "signature": ' + (ConvertTo-JsonString ([string]$Signature.signature)) + ' }'
  }
  $lines += '        }'
  return ($lines -join "`n")
}

function Assert-KeysKnown($obj, [string[]]$allowed, [string]$where) {
  if ($null -eq $obj) { return }
  foreach ($k in @($obj.PSObject.Properties.Name)) {
    if ($allowed -notcontains $k) {
      throw "registry.json $where carries key '$k' that no client DTO maps to (it would be silently ignored)"
    }
  }
}

function Get-DtoContract([string]$dtoPath) {
  # Map "class name -> JsonPropertyName list" straight off the client DTO source, so this
  # check cannot drift from what the client actually reads.
  $text = Get-Content $dtoPath -Raw -Encoding UTF8
  $map = @{}
  foreach ($c in [regex]::Matches($text, 'class\s+(\w+)\s*\{(?<body>[\s\S]*?)\r?\n\}')) {
    $keys = @()
    foreach ($k in [regex]::Matches($c.Groups['body'].Value, 'JsonPropertyName\("([^"]+)"\)')) { $keys += $k.Groups[1].Value }
    if ($keys.Count -gt 0) { $map[$c.Groups[1].Value] = $keys }
  }
  return $map
}

function Get-VersionRank([string]$v) {
  # Best-effort numeric rank used only for a sanity warning, never for correctness.
  $parts = ($v -replace '^v', '') -split '[.\-+]'
  $nums = @()
  foreach ($p in $parts) { if ($p -match '^\d+$') { $nums += [int]$p } else { break } }
  while ($nums.Count -lt 3) { $nums += 0 }
  return ($nums[0] * 1000000 + $nums[1] * 1000 + $nums[2])
}

# ---------------------------------------------------------------- output dir prep

if ([System.IO.Path]::IsPathRooted($OutDir)) { $outAbs = $OutDir } else { $outAbs = Join-Path $repoRoot $OutDir }
$outAbs = [System.IO.Path]::GetFullPath($outAbs)
# Guard: every delete below stays under the repo root.
if (-not $outAbs.StartsWith([System.IO.Path]::GetFullPath($repoRoot), [System.StringComparison]::OrdinalIgnoreCase)) {
  throw "refusing to write outside the repo: $outAbs"
}
if ($Clean -and (Test-Path $outAbs)) {
  Write-Host "clean: removing $outAbs"
  Remove-Item $outAbs -Recurse -Force
}
New-Item -ItemType Directory -Path $outAbs, (Join-Path $outAbs 'packages') -Force | Out-Null

if (-not $PathPrefix) {
  $PathPrefix = (Get-RelativePathCompat $repoRoot $outAbs).Replace('\', '/')
}
$PathPrefix = $PathPrefix.TrimEnd('/')

if ($Sign -and -not $BaseUrl) {
  Write-Host "note: signing in path mode - the packed zips in packages\ carry signature.json too."
}
if ($Sign) {
  $keyPath = if ([System.IO.Path]::IsPathRooted($KeyFile)) { $KeyFile } else { Join-Path $repoRoot $KeyFile }
  if (-not (Test-Path $keyPath)) { throw "-Sign needs a private key file; not found: $keyPath" }
}
if (-not $BaseUrl -and -not $ZipballUrl) {
  Write-Warning "no -ZipballUrl and no -BaseUrl: path-mode entries will rely on the client's built-in official zipball fallback. Only valid for the official warehouse."
}

# ---------------------------------------------------------------- discover plugins

$pluginRoot = Join-Path $repoRoot 'plugins'
$sources = @()
foreach ($dir in Get-ChildItem $pluginRoot -Directory) {
  if ($dir.Name -eq (Split-Path $outAbs -Leaf)) { continue }
  $manifestPath = Join-Path $dir.FullName 'plugin.json'
  if (-not (Test-Path $manifestPath)) { continue }
  $manifest = Get-Content $manifestPath -Raw -Encoding UTF8 | ConvertFrom-Json
  if (-not $manifest.id -or $manifest.id -notmatch '\.') { throw "$($dir.Name): plugin.json 'id' must be reverse-domain style" }
  if (-not $manifest.version) { throw "$($dir.Name): plugin.json 'version' is required" }
  if ($manifest.id -notmatch '^[A-Za-z0-9._-]+$') { throw "$($dir.Name): plugin.json 'id' has characters unsafe for a directory name" }
  if ($manifest.version -notmatch '^[A-Za-z0-9._+-]+$') { throw "$($dir.Name): plugin.json 'version' has characters unsafe for a directory name" }

  $sidecar = $null
  $sidecarPath = Join-Path $dir.FullName 'market.json'
  if (Test-Path $sidecarPath) { $sidecar = Get-Content $sidecarPath -Raw -Encoding UTF8 | ConvertFrom-Json }

  # Assign before the call: a $(...) argument would unroll a single-element tags array into
  # a string, which Assert-Tags rejects as "must be an array".
  $rawTags = $null
  if ($sidecar) { $rawTags = $sidecar.tags }

  $sources += [pscustomobject]@{
    Name     = $dir.Name
    Dir      = $dir.FullName
    Manifest = $manifest
    Sidecar  = $sidecar
    Tags     = (Assert-Tags $manifest.id $rawTags)
  }
}
if ($sources.Count -eq 0) { throw "no plugins found under $pluginRoot" }

# ---------------------------------------------------------------- build native exes

# cargo has no --debug flag: debug is the default profile, only release needs a flag.
# NOTE: build the array via @() around the if — `$x = if (...) { @('a') }` unrolls the
# single-element array into the STRING '--release', and `@x` splatting a string passes its
# individual characters as separate args (cargo then sees a bare `-` and fails).
$profileArgs = @()
if ($Configuration -eq 'release') { $profileArgs = @('--release') }

$nativeSources = @($sources | Where-Object { $_.Manifest.runtime -eq 'native' })
if (-not $SkipBuild -and $nativeSources.Count -gt 0) {
  foreach ($src in $nativeSources) {
    $cargoToml = Join-Path $src.Dir 'Cargo.toml'
    if (-not (Test-Path $cargoToml)) { throw "$($src.Name): native plugin needs Cargo.toml to build its exe" }
    $crate = ([regex]::Match((Get-Content $cargoToml -Raw), '(?m)^\s*name\s*=\s*"([^"]+)"')).Groups[1].Value
    if (-not $crate) { throw "$($src.Name): cannot read package name from Cargo.toml" }
    Write-Host "build: cargo build --$Configuration -p $crate"
    & cargo build @profileArgs -p $crate --manifest-path (Join-Path $repoRoot 'Cargo.toml')
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed for $crate" }
  }
}

# ---------------------------------------------------------------- stage + pack

$excludeNames = @('src', 'target', '.git', 'Cargo.toml', 'Cargo.lock', 'market.json', '.gitignore')
$stageResults = @()

foreach ($src in $sources) {
  $id = $src.Manifest.id
  $ver = $src.Manifest.version
  $pluginDir = Join-Path (Join-Path $outAbs $id) $ver
  # Re-create the version dir from scratch: a stale signature.json left from an earlier
  # signed run would otherwise be copied into a freshly packed zip and look like tampering.
  if (Test-Path $pluginDir) { Remove-Item $pluginDir -Recurse -Force }
  New-Item -ItemType Directory -Path $pluginDir -Force | Out-Null

  foreach ($item in Get-ChildItem $src.Dir -Force) {
    if ($excludeNames -contains $item.Name) { continue }
    if ($src.Manifest.runtime -eq 'native' -and $item.Name -eq $src.Manifest.main) { continue }
    if ($item.PSIsContainer) {
      Copy-Item $item.FullName -Destination (Join-Path $pluginDir $item.Name) -Recurse -Force
    } else {
      Copy-Item $item.FullName -Destination (Join-Path $pluginDir $item.Name) -Force
    }
  }

  if ($src.Manifest.runtime -eq 'native') {
    # cargo writes target/<cfg>/<bin-name>[.exe]; manifest 'main' already carries the .exe
    # extension on Windows, so strip it before re-appending the platform suffix.
    $exeStem = [System.IO.Path]::GetFileNameWithoutExtension($src.Manifest.main)
    $builtExe = Join-Path $repoRoot ("target/$Configuration/" + $exeStem + (Get-ExeSuffix))
    if (-not (Test-Path $builtExe)) {
      throw "$($src.Name): built exe not found at $builtExe (run without -SkipBuild, or check manifest 'main')"
    }
    Copy-Item $builtExe -Destination (Join-Path $pluginDir $src.Manifest.main) -Force
  }

  # Re-read the staged manifest so what we sign/index is literally what ships.
  $stagedManifestPath = Join-Path $pluginDir 'plugin.json'
  if (-not (Test-Path $stagedManifestPath)) { throw "$($src.Name): plugin.json missing after staging" }
  $staged = Get-Content $stagedManifestPath -Raw -Encoding UTF8 | ConvertFrom-Json
  if ($staged.id -ne $id -or $staged.version -ne $ver) { throw "$($src.Name): staged plugin.json id/version mismatch" }

  $stageResults += [pscustomobject]@{
    Name         = $src.Name
    Id           = $id
    Version      = $ver
    Dir          = $pluginDir
    Manifest     = $staged
    Tags         = $src.Tags
    Sidecar      = $src.Sidecar
    PackagePath  = $null
    PackageSha   = $null
    PackageSize  = $null
    Signature    = $null
  }
}

# ---------------------------------------------------------------- sign staged dirs

if ($Sign) {
  Write-Host "sign: cargo build --$Configuration -p spark-sign"
  & cargo build @profileArgs -p spark-sign --manifest-path (Join-Path $repoRoot 'Cargo.toml')
  if ($LASTEXITCODE -ne 0) { throw 'cargo build failed for spark-sign' }
  $signExe = Join-Path $repoRoot ("target/$Configuration/spark-sign" + (Get-ExeSuffix))
  if (-not (Test-Path $signExe)) { throw "spark-sign not found at $signExe" }
  foreach ($r in $stageResults) {
    Write-Host "sign: $($r.Id) $($r.Version) (key_id=$KeyId)"
    & $signExe sign --dir $r.Dir --key $keyPath --key-id $KeyId
    if ($LASTEXITCODE -ne 0) { throw "signing failed for $($r.Id)" }
    $sigPath = Join-Path $r.Dir 'signature.json'
    if (-not (Test-Path $sigPath)) { throw "spark-sign produced no signature.json for $($r.Id)" }
    # Read it back from disk: the index must advertise what was actually written.
    $r.Signature = Get-Content $sigPath -Raw -Encoding UTF8 | ConvertFrom-Json
    if (-not $r.Signature.signature -or -not $r.Signature.key_id) { throw "signature.json for $($r.Id) is incomplete" }
  }
}

# ---------------------------------------------------------------- pack zips

Add-Type -AssemblyName System.IO.Compression.FileSystem
foreach ($r in $stageResults) {
  $zip = Join-Path (Join-Path $outAbs 'packages') ("$($r.Id)-$($r.Version).spark-plugin")
  if (Test-Path $zip) { Remove-Item $zip -Force }
  # ZipFile.CreateFromDirectory puts the directory *contents* at the archive root, which is
  # one of the two layouts the client's ExtractZipSafely accepts (root plugin.json).
  [System.IO.Compression.ZipFile]::CreateFromDirectory($r.Dir, $zip, [System.IO.Compression.CompressionLevel]::Optimal, $false)
  $r.PackagePath = $zip
  $r.PackageSha = (Get-FileHash $zip -Algorithm SHA256).Hash.ToLowerInvariant()
  $r.PackageSize = (Get-Item $zip).Length

  # Self-check the pack against the client contract before it can be published.
  $archive = [System.IO.Compression.ZipFile]::OpenRead($zip)
  try {
    $names = @($archive.Entries | ForEach-Object { $_.FullName })
    if (-not ($names -contains 'plugin.json')) { throw "packed zip for $($r.Id) has no root plugin.json" }
    if ($Sign -and -not ($names -contains 'signature.json')) { throw "packed zip for $($r.Id) is missing signature.json (sign before pack)" }
  } finally { $archive.Dispose() }
}

# ---------------------------------------------------------------- registry.json

$sb = New-Object System.Text.StringBuilder
[void]$sb.Append("{`n")
[void]$sb.Append('  "schema": 1,'); [void]$sb.Append("`n")
[void]$sb.Append('  "name": ' + (ConvertTo-JsonString $RegistryName) + ','); [void]$sb.Append("`n")
if ($ZipballUrl) { [void]$sb.Append('  "zipball_url": ' + (ConvertTo-JsonString $ZipballUrl) + ','); [void]$sb.Append("`n") }
[void]$sb.Append('  "updated": ' + (ConvertTo-JsonString ((Get-Date).ToUniversalTime().ToString('yyyy-MM-ddTHH:mm:ssZ'))) + ','); [void]$sb.Append("`n")
[void]$sb.Append('  "plugins": ['); [void]$sb.Append("`n")

for ($i = 0; $i -lt $stageResults.Count; $i++) {
  $r = $stageResults[$i]
  $m = $r.Manifest
  [void]$sb.Append("    {`n")
  [void]$sb.Append('      "id": ' + (ConvertTo-JsonString $r.Id) + ','); [void]$sb.Append("`n")
  [void]$sb.Append('      "name": ' + (ConvertTo-JsonString ([string]$m.name)) + ','); [void]$sb.Append("`n")
  [void]$sb.Append('      "description": ' + (ConvertTo-JsonString ([string]$m.description)) + ','); [void]$sb.Append("`n")
  [void]$sb.Append('      "author": ' + (ConvertTo-JsonString ([string]$m.author)) + ','); [void]$sb.Append("`n")

  $homepage = if ($r.Sidecar -and $r.Sidecar.homepage) { $r.Sidecar.homepage } else { $m.homepage }
  $icon = if ($r.Sidecar -and $r.Sidecar.icon) { $r.Sidecar.icon } else { $m.icon }
  if ($homepage) { [void]$sb.Append('      "homepage": ' + (ConvertTo-JsonString ([string]$homepage)) + ','); [void]$sb.Append("`n") }
  if ($icon) { [void]$sb.Append('      "icon": ' + (ConvertTo-JsonString ([string]$icon)) + ','); [void]$sb.Append("`n") }

  [void]$sb.Append('      "runtime": ' + (ConvertTo-JsonString ([string]$m.runtime)) + ','); [void]$sb.Append("`n")

  $perms = @()
  if ($m.permissions) { $perms = @($m.permissions) }
  $permJson = if ($perms.Count -gt 0) { ($perms | ForEach-Object { ConvertTo-JsonString ([string]$_) }) -join ', ' } else { '' }
  [void]$sb.Append('      "permissions": [' + $permJson + '],'); [void]$sb.Append("`n")

  $tagJson = if ($r.Tags.Count -gt 0) { ($r.Tags | ForEach-Object { ConvertTo-JsonString $_ }) -join ', ' } else { '' }
  [void]$sb.Append('      "tags": [' + $tagJson + '],'); [void]$sb.Append("`n")
  [void]$sb.Append('      "latest": ' + (ConvertTo-JsonString $r.Version) + ','); [void]$sb.Append("`n")
  [void]$sb.Append('      "versions": ['); [void]$sb.Append("`n")

  $versionEntries = @()
  $versionEntries += (Format-VersionEntry -Version $r.Version `
    -PathValue $(if ($BaseUrl) { $null } else { "$PathPrefix/$($r.Id)/$($r.Version)" }) `
    -Url $(if ($BaseUrl) { $BaseUrl.TrimEnd('/') + '/packages/' + [System.IO.Path]::GetFileName($r.PackagePath) } else { $null }) `
    -Sha $(if ($BaseUrl) { $r.PackageSha } else { $null }) `
    -Size $(if ($BaseUrl) { $r.PackageSize } else { $null }) `
    -Released ((Get-Date).ToUniversalTime().ToString('yyyy-MM-dd')) `
    -Signature $r.Signature)

  # Version dirs staged by earlier runs are kept and listed as path-only entries: the
  # warehouse serves them from the zipball, and dropping them would break pinned installs.
  $idDir = Join-Path $outAbs $r.Id
  $older = @()
  if (Test-Path $idDir) {
    $older = @(Get-ChildItem $idDir -Directory | Where-Object { $_.Name -ne $r.Version } | Sort-Object Name -Descending)
  }
  foreach ($d in $older) {
    if ((Get-VersionRank $d.Name) -gt (Get-VersionRank $r.Version)) {
      Write-Warning "$($r.Id): staged version dir '$($d.Name)' ranks higher than the packed '$($r.Version)' - packing an older branch?"
    }
    $released = (Get-Date $d.LastWriteTimeUtc).ToString('yyyy-MM-dd')
    # Older dirs may still carry a signature.json from the run that staged them.
    $oldSig = $null
    $oldSigPath = Join-Path $d.FullName 'signature.json'
    if (Test-Path $oldSigPath) { $oldSig = Get-Content $oldSigPath -Raw -Encoding UTF8 | ConvertFrom-Json }
    $versionEntries += (Format-VersionEntry -Version $d.Name `
      -PathValue "$PathPrefix/$($r.Id)/$($d.Name)" -Url $null -Sha $null -Size $null `
      -Released $released -Signature $oldSig)
  }

  [void]$sb.Append(($versionEntries -join ",`n")); [void]$sb.Append("`n")
  [void]$sb.Append('      ]'); [void]$sb.Append("`n")
  [void]$sb.Append('    }')
  if ($i -lt $stageResults.Count - 1) { [void]$sb.Append(',') }
  [void]$sb.Append("`n")
}
[void]$sb.Append('  ]'); [void]$sb.Append("`n")
[void]$sb.Append("}"); [void]$sb.Append("`n")

$registryPath = Join-Path $outAbs 'registry.json'
Write-Utf8NoBom $registryPath $sb.ToString()

# Round-trip check: proves the hand-written JSON is valid and that non-ASCII (tags, names)
# survived as UTF-8 rather than being mangled by the writer.
$parsed = Get-Content $registryPath -Raw -Encoding UTF8 | ConvertFrom-Json
if ($parsed.schema -ne 1) { throw 'generated registry.json failed its own schema check' }
if (@($parsed.plugins).Count -ne $stageResults.Count) { throw 'generated registry.json plugin count mismatch' }
foreach ($r in $stageResults) {
  $entry = $parsed.plugins | Where-Object { $_.id -eq $r.Id }
  if (-not $entry) { throw "generated registry.json is missing $($r.Id)" }
  if (-not $entry.latest) { throw "generated registry.json entry $($r.Id) has no latest" }
  if (@($entry.versions)[0].version -ne $r.Version) { throw "generated registry.json entry $($r.Id) latest version is not first" }
  if ($r.Tags.Count -gt 0 -and (@($entry.tags) -join '|') -ne ($r.Tags -join '|')) { throw "generated registry.json tags mismatch for $($r.Id)" }
  if ($Sign -and -not @($entry.versions)[0].signature) { throw "generated registry.json entry $($r.Id) lost its signature summary" }
  # Every recorded path must resolve on disk to a real plugin dir: that is exactly what the
  # client extracts from the zipball, so a wrong PathPrefix would break installs silently.
  foreach ($v in @($entry.versions)) {
    if (-not $v.path) { continue }
    $onDisk = Join-Path $repoRoot ($v.path.Replace('/', '\'))
    if (-not (Test-Path $onDisk)) { throw "registry.json path does not exist on disk: $($v.path)" }
    if (-not (Test-Path (Join-Path $onDisk 'plugin.json'))) { throw "version dir has no plugin.json: $($v.path)" }
  }
  # Fields the client branches on must actually be present.
  if (-not $entry.runtime) { throw "generated registry.json entry $($r.Id) has no runtime" }
  if (-not @($entry.versions)[0].version) { throw "generated registry.json entry $($r.Id) version entry has no version" }
}

# Field-name contract check against the client DTOs: a typo here (zipballUrl vs zipball_url)
# would not fail anything at runtime - the client would just silently ignore the field.
$dtoPath = Join-Path $repoRoot 'ui/Spark.UI/Models/RegistryDto.cs'
if (Test-Path $dtoPath) {
  $contract = Get-DtoContract $dtoPath
  if ($contract['RegistryIndexDto'] -and $contract['RegistryPluginDto'] -and $contract['RegistryVersionDto']) {
    Assert-KeysKnown $parsed $contract['RegistryIndexDto'] 'root'
    foreach ($p in @($parsed.plugins)) {
      Assert-KeysKnown $p $contract['RegistryPluginDto'] "plugins[$($p.id)]"
      foreach ($v in @($p.versions)) {
        Assert-KeysKnown $v $contract['RegistryVersionDto'] "plugins[$($p.id)].versions[$($v.version)]"
        if ($v.signature) {
          Assert-KeysKnown $v.signature $contract['RegistrySignatureDto'] "plugins[$($p.id)].versions[$($v.version)].signature"
        }
      }
    }
    Write-Host 'check: registry.json keys all map to client DTOs (RegistryDto.cs)'
  } else {
    Write-Warning 'could not parse the client DTO contract from RegistryDto.cs; field-name check skipped'
  }
} else {
  Write-Host "note: $dtoPath not found (warehouse-only checkout); field-name check skipped"
}

# ---------------------------------------------------------------- optional warehouse check

if ($Sign) {
  $signExe = Join-Path $repoRoot ("target/$Configuration/spark-sign" + (Get-ExeSuffix))
  Write-Host 'check: spark-sign check-registry (every version must carry a valid official signature)'
  & $signExe check-registry --registry $registryPath --dir $repoRoot
  if ($LASTEXITCODE -ne 0) { throw 'check-registry failed: the warehouse is not admission-ready' }
}

# ---------------------------------------------------------------- summary

Write-Host ''
Write-Host "packed $($stageResults.Count) plugin(s) into $outAbs"
foreach ($r in $stageResults) {
  $tags = if ($r.Tags.Count -gt 0) { $r.Tags -join ',' } else { '(none)' }
  $kb = [math]::Round($r.PackageSize / 1024, 1)
  Write-Host ("  {0,-22} {1,-10} {2,8} KB  sha256={3}  tags={4}" -f $r.Id, $r.Version, $kb, $r.PackageSha.Substring(0, 12), $tags)
}
Write-Host "  registry.json: $registryPath"
Write-Host ("  mode: " + $(if ($BaseUrl) { "direct-download (url+sha256), BaseUrl=$BaseUrl" } else { "path (versions[].path=$PathPrefix/<id>/<version>)" }))
if (-not $Sign) {
  Write-Host '  signed: NO - run again with -Sign -KeyFile <key> to produce signature.json (sign before pack).'
}
