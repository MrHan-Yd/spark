# Diagnose which Spark processes are running and from where. ASCII only.
$procs = Get-Process | Where-Object { $_.ProcessName -match 'spark' }
if ($null -eq $procs) {
    Write-Output 'NO_SPARK_PROCESSES'
} else {
    foreach ($p in $procs) {
        try { $path = $p.Path } catch { $path = '<no-access>' }
        Write-Output ("PID={0} NAME={1} PATH={2}" -f $p.Id, $p.ProcessName, $path)
    }
}