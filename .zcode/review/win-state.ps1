# Check Spark UI main window visibility. ASCII only.
Add-Type -TypeDefinition 'using System;using System.Runtime.InteropServices;public class W{[DllImport("user32.dll")]public static extern bool IsWindowVisible(IntPtr h);[DllImport("user32.dll")]public static extern bool IsIconic(IntPtr h);[DllImport("user32.dll")]public static extern IntPtr GetForegroundWindow();[DllImport("user32.dll")]public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);}'
$p = Get-Process -Name Spark -ErrorAction SilentlyContinue | Select-Object -First 1
if (-not $p) { Write-Output 'NO_SPARK_UI'; exit }
$h = $p.MainWindowHandle
$pid2 = 0
$fg = [W]::GetForegroundWindow()
[W]::GetWindowThreadProcessId($fg, [ref]$pid2) | Out-Null
Write-Output ("handle={0} visible={1} iconic={2} fgpid={3} uipid={4}" -f $h, [W]::IsWindowVisible($h), [W]::IsIconic($h), $pid2, $p.Id)