# Probe: after summon, list visible windows of UI process. ASCII only.
Add-Type -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Text;
public class Win32P {
    [DllImport("user32.dll")] public static extern void keybd_event(byte vk, byte scan, uint flags, UIntPtr extra);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
    [DllImport("user32.dll")] public static extern int GetWindowText(IntPtr h, StringBuilder sb, int max);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
    [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
    public delegate bool EnumProc(IntPtr h, IntPtr l);
    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc cb, IntPtr l);
    public static List<IntPtr> OfPid(uint pid) {
        var list = new List<IntPtr>();
        EnumWindows((h, l) => {
            uint p; GetWindowThreadProcessId(h, out p);
            if (p == pid) list.Add(h);
            return true;
        }, IntPtr.Zero);
        return list;
    }
}
'@
function Tap([byte]$vk) { [Win32P]::keybd_event($vk, 0, 0, [UIntPtr]::Zero) }
function Untap([byte]$vk) { [Win32P]::keybd_event($vk, 0, 2, [UIntPtr]::Zero) }
function Press([byte]$vk) { Tap $vk; Start-Sleep -Milliseconds 40; Untap $vk }
function WindowTitle($h) {
    $sb = New-Object System.Text.StringBuilder 256
    [Win32P]::GetWindowText($h, $sb, 256) | Out-Null
    return $sb.ToString()
}
$ui = Get-Process -Name Spark | Select-Object -First 1
$uiPid = [uint32]$ui.Id
$fgBefore = [Win32P]::GetForegroundWindow()
$fgPidBefore = 0; [Win32P]::GetWindowThreadProcessId($fgBefore, [ref]$fgPidBefore) | Out-Null
Write-Output ("before: fgpid={0} fgtitle={1}" -f $fgPidBefore, (WindowTitle $fgBefore))
Tap 0x12; Start-Sleep -Milliseconds 50; Press 0x20; Start-Sleep -Milliseconds 50; Untap 0x12
Start-Sleep -Milliseconds 1800
$fg = [Win32P]::GetForegroundWindow()
$fgPid = 0; [Win32P]::GetWindowThreadProcessId($fg, [ref]$fgPid) | Out-Null
Write-Output ("after: fgpid={0} fgtitle={1}" -f $fgPid, (WindowTitle $fg))
Write-Output ("MainWindowHandle={0}" -f $ui.MainWindowHandle)
foreach ($h in [Win32P]::OfPid($uiPid)) {
    $vis = [Win32P]::IsWindowVisible($h)
    Write-Output ("hwnd=0x{0:X} visible={1} title={2}" -f $h.ToInt64(), $vis, (WindowTitle $h))
}