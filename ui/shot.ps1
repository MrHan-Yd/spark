Add-Type -AssemblyName System.Drawing
Add-Type @"
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Text;
public struct RECT { public int Left, Top, Right, Bottom; }
public class W2 {
    public delegate bool EnumProc(IntPtr hWnd, IntPtr lParam);
    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc cb, IntPtr lParam);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr hWnd, out uint pid);
    [DllImport("user32.dll")] public static extern int GetWindowTextLength(IntPtr hWnd);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetWindowText(IntPtr hWnd, StringBuilder sb, int max);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr hWnd, out RECT rect);
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr hWnd, int cmd);
    public static List<IntPtr> FindByPid(uint target, bool visibleOnly) {
        var list = new List<IntPtr>();
        EnumWindows((h, l) => {
            uint pid; GetWindowThreadProcessId(h, out pid);
            if (pid == target && (!visibleOnly || IsWindowVisible(h))) list.Add(h);
            return true;
        }, IntPtr.Zero);
        return list;
    }
}
"@

$exe = 'D:\demo\test01\spark\ui\Spark.UI\bin\Debug\net8.0-windows10.0.19041.0\win-x64\spark-ui.exe'
$p = Get-Process spark-ui -ErrorAction SilentlyContinue | Select-Object -First 1
if (-not $p) {
    Start-Process -FilePath $exe -WorkingDirectory (Split-Path $exe)
    Start-Sleep -Seconds 4
    $p = Get-Process spark-ui -ErrorAction SilentlyContinue | Select-Object -First 1
}
$hws = [W2]::FindByPid([uint32]$p.Id, $true)
if ($hws.Count -eq 0) {
    # 触发 toggle 显示窗口
    Start-Process -FilePath $exe -WorkingDirectory (Split-Path $exe)
    Start-Sleep -Seconds 2
    $hws = [W2]::FindByPid([uint32]$p.Id, $true)
}
if ($hws.Count -eq 0) { Write-Host 'still no visible window'; exit 2 }
$h = $hws[0]
[W2]::SetForegroundWindow($h) | Out-Null
[W2]::ShowWindow($h, 9) | Out-Null
Start-Sleep -Seconds 2
$r = New-Object RECT
[W2]::GetWindowRect($h, [ref]$r) | Out-Null
$w = $r.Right - $r.Left; $hh = $r.Bottom - $r.Top
Write-Host ("window ${w}x${hh} at $($r.Left),$($r.Top)")
$bmp = New-Object System.Drawing.Bitmap($w, $hh)
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.CopyFromScreen($r.Left, $r.Top, 0, 0, $bmp.Size)
$bmp.Save('D:\demo\test01\spark\ui\shot.png', [System.Drawing.Imaging.ImageFormat]::Png)
$g.Dispose(); $bmp.Dispose()
Write-Host 'saved shot.png'
