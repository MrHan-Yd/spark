Add-Type @"
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Text;
public class W {
    public delegate bool EnumProc(IntPtr hWnd, IntPtr lParam);
    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc cb, IntPtr lParam);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr hWnd, out uint pid);
    [DllImport("user32.dll")] public static extern int GetWindowTextLength(IntPtr hWnd);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetWindowText(IntPtr hWnd, StringBuilder sb, int max);
    public static List<string> FindByPid(uint target) {
        var list = new List<string>();
        EnumWindows((h, l) => {
            uint pid; GetWindowThreadProcessId(h, out pid);
            if (pid == target) {
                int len = GetWindowTextLength(h);
                var sb = new StringBuilder(len + 1);
                GetWindowText(h, sb, len + 1);
                list.Add(h.ToString() + "|" + IsWindowVisible(h) + "|" + sb.ToString());
            }
            return true;
        }, IntPtr.Zero);
        return list;
    }
}
"@
$p = Get-Process spark-ui -ErrorAction SilentlyContinue | Select-Object -First 1
Write-Host ("pid=" + $p.Id)
[W]::FindByPid([uint32]$p.Id) | ForEach-Object { Write-Host $_ }
