# Probe: (1) can UIA see hidden WinUI3 window tree? (2) who steals foreground after summon?
# ASCII only.
Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
using System.Text;
public class W32Q {
    [DllImport("user32.dll")] public static extern void keybd_event(byte vk, byte scan, uint flags, UIntPtr extra);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
    [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
}
'@
function Tap([byte]$vk) { [W32Q]::keybd_event($vk, 0, 0, [UIntPtr]::Zero) }
function Untap([byte]$vk) { [W32Q]::keybd_event($vk, 0, 2, [UIntPtr]::Zero) }
function Press([byte]$vk) { Tap $vk; Start-Sleep -Milliseconds 40; Untap $vk }
function FgInfo {
    $fg = [W32Q]::GetForegroundWindow()
    $p = 0; [W32Q]::GetWindowThreadProcessId($fg, [ref]$p) | Out-Null
    $pn = '?'
    try { $pn = (Get-Process -Id $p -ErrorAction Stop).ProcessName } catch {}
    return ("hwnd=0x{0:X} pid={1} name={2}" -f $fg.ToInt64(), $p, $pn)
}
$auto = [System.Windows.Automation.AutomationElement]
$ui = Get-Process -Name Spark | Select-Object -First 1
$uiPid = [int]$ui.Id

Write-Output ("fg before: " + (FgInfo))
# 1) hidden-window UIA query attempt (no summon)
$pidCond = New-Object System.Windows.Automation.PropertyCondition($auto::ProcessIdProperty, $uiPid)
$winCond = New-Object System.Windows.Automation.PropertyCondition($auto::ControlTypeProperty, [System.Windows.Automation.ControlType]::Window)
$both = New-Object System.Windows.Automation.AndCondition($pidCond, $winCond)
$wins = $auto::RootElement.FindAll([System.Windows.Automation.TreeScope]::Children, $both)
Write-Output ("hidden-state: uia windows of pid = {0}" -f $wins.Count)
foreach ($w in $wins) {
    Write-Output ("  win name='{0}' offscreen={1}" -f $w.Current.Name, $w.Current.IsOffscreen)
    $nav = $w.FindFirst([System.Windows.Automation.TreeScope]::Descendants, (New-Object System.Windows.Automation.PropertyCondition($auto::AutomationIdProperty, 'NavPlugins')))
    $btn = $w.FindFirst([System.Windows.Automation.TreeScope]::Descendants, (New-Object System.Windows.Automation.PropertyCondition($auto::AutomationIdProperty, 'BtnSettings')))
    Write-Output ("    NavPlugins={0} BtnSettings={1}" -f ($null -ne $nav), ($null -ne $btn))
}

# 2) summon and watch foreground for 4s
Tap 0x12; Start-Sleep -Milliseconds 50; Press 0x20; Start-Sleep -Milliseconds 50; Untap 0x12
for ($i = 1; $i -le 10; $i++) {
    Start-Sleep -Milliseconds 400
    Write-Output ("t+{0}s: {1}" -f ($i * 0.4), (FgInfo))
}