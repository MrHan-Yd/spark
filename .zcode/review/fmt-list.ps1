# Pretty-print host plugin list. ASCII only.
$raw = [Console]::In.ReadToEnd()
$j = $raw | ConvertFrom-Json
foreach ($p in $j.result) {
    Write-Output ("id={0} name={1} enabled={2} has_page={3} runtime={4}" -f $p.id, $p.name, $p.enabled, $p.has_page, $p.runtime)
    Write-Output ("  icon={0}" -f $p.icon)
}