# Probe: pinpoint where the UIA chain breaks. ASCII only.
Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes
Add-Type -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Text;
public class W32R {
    [DllImport("user32.dll")] public static extern void keybd_event(byte vk, byte scan, uint flags, UIntPtr extra);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
    public delegate bool EnumProc(IntPtr h, IntPtr l);
    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc cb, IntPtr l);
    public static List<IntPtr> OfPid(uint pid) {
        var list = new List<IntPtr>();
        EnumWindows((h, l) => {
            uint p; GetWindowThreadProcessId(h, out p);
            if (p == pid && IsWindowVisible(h)) list.Add(h);
            return true;
        }, IntPtr.Zero);
        return list;
    }
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
}
'@
function Tap([byte]$vk) { [W32R]::keybd_event($vk, 0, 0, [UIntPtr]::Zero) }
function Untap([byte]$vk) { [W32R]::keybd_event($vk, 0, 2, [UIntPtr]::Zero) }
function Press([byte]$vk) { Tap $vk; Start-Sleep -Milliseconds 40; Untap $vk }
$auto = [System.Windows.Automation.AutomationElement]
$ui = Get-Process -Name Spark | Select-Object -First 1
$uiPid = [int]$ui.Id

$launcherH = [IntPtr]::Zero
foreach ($h in [W32R]::OfPid($uiPid)) { $launcherH = $h; break }
if ($launcherH -eq [IntPtr]::Zero) {
    Tap 0x12; Start-Sleep -Milliseconds 50; Press 0x20; Start-Sleep -Milliseconds 50; Untap 0x12
    Start-Sleep -Milliseconds 1500
    foreach ($h in [W32R]::OfPid($uiPid)) { $launcherH = $h; break }
}
Write-Output ("launcher hwnd=0x{0:X}" -f $launcherH.ToInt64())

Tap 0x11; Start-Sleep -Milliseconds 50; Press 0xBC; Start-Sleep -Milliseconds 50; Untap 0x11
Start-Sleep -Milliseconds 1500

$pidCond = New-Object System.Windows.Automation.PropertyCondition($auto::ProcessIdProperty, $uiPid)
$winCond = New-Object System.Windows.Automation.PropertyCondition($auto::ControlTypeProperty, [System.Windows.Automation.ControlType]::Window)
$both = New-Object System.Windows.Automation.AndCondition($pidCond, $winCond)
$wins = $auto::RootElement.FindAll([System.Windows.Automation.TreeScope]::Children, $both)
Write-Output ("uia windows of pid: {0}" -f $wins.Count)
foreach ($w in $wins) {
    Write-Output ("  win name='{0}' class='{1}' offscreen={2}" -f $w.Current.Name, $w.Current.ClassName, $w.Current.IsOffscreen)
}
$anyPid = $auto::RootElement.FindAll([System.Windows.Automation.TreeScope]::Children, $pidCond)
Write-Output ("uia children of pid (any type): {0}" -f $anyPid.Count)
foreach ($w in $anyPid) {
    Write-Output ("  el type={0} name='{1}' class='{2}' id='{3}'" -f $w.Current.ControlType.ProgrammaticName, $w.Current.Name, $w.Current.ClassName, $w.Current.AutomationId)
}
if ($anyPid.Count -gt 0) {
    $w0 = $anyPid[0]
    foreach ($aid in @('BtnSettings', 'NavPlugins', 'PluginList', 'QueryBox')) {
        $e = $w0.FindFirst([System.Windows.Automation.TreeScope]::Descendants, (New-Object System.Windows.Automation.PropertyCondition($auto::AutomationIdProperty, $aid)))
        Write-Output ("  aid '{0}' found={1}" -f $aid, ($null -ne $e))
    }
}