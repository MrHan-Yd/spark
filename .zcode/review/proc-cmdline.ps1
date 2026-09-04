# Check Spark process command lines and start times. ASCII only.
Get-CimInstance Win32_Process | Where-Object { $_.Name -match 'spark' } | ForEach-Object {
    Write-Output ("PID={0} NAME={1} START={2}" -f $_.ProcessId, $_.Name, $_.CreationDate)
    Write-Output ("  CMDLINE={0}" -f $_.CommandLine)
}