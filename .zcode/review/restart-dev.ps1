# Rebuild UI with the spawn fix and restart dev host+UI correctly. ASCII only.
$ErrorActionPreference = 'Stop'
Set-Location 'D:\demo\test01\spark'

Write-Output '== stop old Spark + spark-host =='
Get-Process Spark, spark-host -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Milliseconds 600

Write-Output '== dotnet build =='
dotnet build 'ui\Spark.UI\Spark.UI.csproj' -c Debug
if ($LASTEXITCODE -ne 0) { throw 'build failed' }

$exe = 'ui\Spark.UI\bin\Debug\net8.0-windows10.0.19041.0\win-x64\Spark.exe'
if (-not (Test-Path $exe)) { throw "missing $exe" }

Write-Output '== start host (repo plugins dir) =='
Start-Process -FilePath 'D:\demo\test01\spark\target\debug\spark-host.exe' `
    -ArgumentList '--no-ui', '--plugins-dir', 'D:\demo\test01\spark\plugins' `
    -WorkingDirectory 'D:\demo\test01\spark' -WindowStyle Hidden
Start-Sleep -Milliseconds 1500

Write-Output '== start UI =='
Start-Process -FilePath $exe -WorkingDirectory (Split-Path $exe -Parent)
Start-Sleep -Seconds 3

Get-CimInstance Win32_Process | Where-Object { $_.Name -match 'spark' } | ForEach-Object {
    Write-Output ("PID={0} NAME={1} CMD={2}" -f $_.ProcessId, $_.Name, $_.CommandLine)
}