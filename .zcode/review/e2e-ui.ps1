# UI-level e2e for native plugin page model. ASCII only.
# Chain: summon -> settings -> plugins pane -> expand Echo card -> open page
#        -> click send in WebView (spark.rpc) -> lazy spawn -> close window -> exe exit.
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes
Add-Type -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Text;
public class Win32U {
    [DllImport("user32.dll")] public static extern void keybd_event(byte vk, byte scan, uint flags, UIntPtr extra);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
    [DllImport("user32.dll")] public static extern int GetWindowText(IntPtr h, StringBuilder sb, int max);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
    [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint msg, IntPtr w, IntPtr l);
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
}
'@

function Tap([byte]$vk) { [Win32U]::keybd_event($vk, 0, 0, [UIntPtr]::Zero) }
function Untap([byte]$vk) { [Win32U]::keybd_event($vk, 0, 2, [UIntPtr]::Zero) }
function Press([byte]$vk) { Tap $vk; Start-Sleep -Milliseconds 40; Untap $vk }

function WindowTitle($h) {
    $sb = New-Object System.Text.StringBuilder 256
    [Win32U]::GetWindowText($h, $sb, 256) | Out-Null
    return $sb.ToString()
}

# Chinese literals via codepoints (script file must stay ASCII).
$sOpen = [string][char]0x6253 + [char]0x5F00          # open
$sSend = [string][char]0x53D1 + [char]0x9001          # send button in page.html
$auto = [System.Windows.Automation.AutomationElement]

# button name may aggregate FontIcon glyph + text, so locate via text element then walk up
function Find-ButtonByText($scope, $text) {
    $walker = [System.Windows.Automation.TreeWalker]::ControlViewWalker
    $t = $scope.FindFirst([System.Windows.Automation.TreeScope]::Descendants, (New-Object System.Windows.Automation.PropertyCondition($auto::NameProperty, $text)))
    while ($t) {
        if ($t.Current.ControlType -eq [System.Windows.Automation.ControlType]::Button) { return $t }
        $t = $walker.GetParent($t)
    }
    return $null
}

$ui = Get-Process -Name Spark -ErrorAction Stop | Select-Object -First 1
$uiPid = [int]$ui.Id
Write-Output "UI pid=$uiPid"

# -- 1. summon launcher via global hotkey Alt+Space (skip if already visible; retry: re-hidden races)
$launcherH = [IntPtr]::Zero
foreach ($h in [Win32U]::OfPid($uiPid)) {
    if ([Win32U]::IsWindowVisible($h) -and (WindowTitle $h) -eq 'Spark') { $launcherH = $h; break }
}
for ($try = 0; $try -lt 6 -and $launcherH -eq [IntPtr]::Zero; $try++) {
    Tap 0x12; Start-Sleep -Milliseconds 50; Press 0x20; Start-Sleep -Milliseconds 50; Untap 0x12
    for ($i = 0; $i -lt 8; $i++) {
        Start-Sleep -Milliseconds 250
        foreach ($h in [Win32U]::OfPid($uiPid)) {
            if ([Win32U]::IsWindowVisible($h) -and (WindowTitle $h) -eq 'Spark') { $launcherH = $h; break }
        }
        if ($launcherH -ne [IntPtr]::Zero) { break }
    }
}
if ($launcherH -eq [IntPtr]::Zero) { throw 'FAIL summon: launcher window not visible after Alt+Space retries' }
Write-Output 'PASS summon: launcher visible'

# -- 2/3. open settings + navigate to plugins pane via UIA (keyboard accelerator unreliable)
function Invoke-UiaFlow {
    $root = $auto::RootElement
    $pidCond = New-Object System.Windows.Automation.PropertyCondition($auto::ProcessIdProperty, $uiPid)
    $wins = $root.FindAll([System.Windows.Automation.TreeScope]::Children, $pidCond)
    $main = $null
    foreach ($w in $wins) {
        if ($w.Current.ClassName -eq 'WinUIDesktopWin32WindowClass') { $main = $w; break }
    }
    if (-not $main) { throw 'FAIL uia: launcher window element not found' }
    $aidCond = { param($id) New-Object System.Windows.Automation.PropertyCondition($auto::AutomationIdProperty, $id) }
    $btnSettings = $main.FindFirst([System.Windows.Automation.TreeScope]::Descendants, (& $aidCond 'BtnSettings'))
    if (-not $btnSettings) { throw 'FAIL uia: BtnSettings not found' }
    ($btnSettings.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern)).Invoke()
    Start-Sleep -Milliseconds 1200
    $navEl = $main.FindFirst([System.Windows.Automation.TreeScope]::Descendants, (& $aidCond 'NavPlugins'))
    if (-not $navEl) { throw 'FAIL uia: NavPlugins not found (settings page did not open?)' }
    Write-Output 'PASS uia: settings pane reachable (NavPlugins found)'
    ($navEl.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern)).Invoke()
    Start-Sleep -Milliseconds 1000

    $list = $main.FindFirst([System.Windows.Automation.TreeScope]::Descendants, (& $aidCond 'PluginList'))
    if (-not $list) { throw 'FAIL uia: PluginList not found' }
    $echoName = New-Object System.Windows.Automation.PropertyCondition($auto::NameProperty, 'Echo')
    $items = $list.FindAll([System.Windows.Automation.TreeScope]::Descendants, (New-Object System.Windows.Automation.PropertyCondition($auto::ControlTypeProperty, [System.Windows.Automation.ControlType]::ListItem)))
    $echoItem = $null
    foreach ($it in $items) {
        if ($it.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $echoName)) { $echoItem = $it; break }
    }
    if (-not $echoItem) { throw 'FAIL uia: Echo row not found in PluginList' }
    try { ($echoItem.GetCurrentPattern([System.Windows.Automation.ScrollItemPattern]::Pattern)).ScrollIntoView() } catch {}
    Start-Sleep -Milliseconds 400

    # expand chevron: Button supporting Invoke but not Toggle, before expansion the only such button
    $btnType = New-Object System.Windows.Automation.PropertyCondition($auto::ControlTypeProperty, [System.Windows.Automation.ControlType]::Button)
    $btns = $echoItem.FindAll([System.Windows.Automation.TreeScope]::Descendants, $btnType)
    $chevron = $null
    foreach ($b in $btns) {
        $hasToggle = $false
        try { $null = $b.GetCurrentPattern([System.Windows.Automation.TogglePattern]::Pattern); $hasToggle = $true } catch {}
        if (-not $hasToggle) { $chevron = $b; break }
    }
    if (-not $chevron) { throw 'FAIL uia: expand chevron not found on Echo card' }
    ($chevron.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern)).Invoke()
    Start-Sleep -Milliseconds 900

    $openBtn = Find-ButtonByText $echoItem $sOpen
    if (-not $openBtn) { throw "FAIL uia: '$sOpen' button not visible on Echo card (HasPage wiring?)" }
    ($openBtn.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern)).Invoke()
    Write-Output 'PASS card: expand + open button invoked (HasPage button present)'
}

$uiaOk = $false
for ($pass = 1; $pass -le 3 -and -not $uiaOk; $pass++) {
    try { Invoke-UiaFlow; $uiaOk = $true }
    catch {
        Write-Output ("retry pass {0}: {1}" -f $pass, $_.Exception.Message)
        $launcherH = [IntPtr]::Zero
        foreach ($h in [Win32U]::OfPid($uiPid)) {
            if ([Win32U]::IsWindowVisible($h) -and (WindowTitle $h) -eq 'Spark') { $launcherH = $h; break }
        }
        for ($try = 0; $try -lt 4 -and $launcherH -eq [IntPtr]::Zero; $try++) {
            Tap 0x12; Start-Sleep -Milliseconds 50; Press 0x20; Start-Sleep -Milliseconds 50; Untap 0x12
            for ($i = 0; $i -lt 8; $i++) {
                Start-Sleep -Milliseconds 250
                foreach ($h in [Win32U]::OfPid($uiPid)) {
                    if ([Win32U]::IsWindowVisible($h) -and (WindowTitle $h) -eq 'Spark') { $launcherH = $h; break }
                }
                if ($launcherH -ne [IntPtr]::Zero) { break }
            }
        }
        if ($launcherH -eq [IntPtr]::Zero) { throw 'FAIL summon: launcher lost and re-summon failed' }
        Start-Sleep -Milliseconds 400
    }
}
if (-not $uiaOk) { throw 'FAIL uia: card open flow failed after 3 passes' }

# -- 4. wait for plugin page window titled 'Echo'
$echoH = [IntPtr]::Zero
for ($i = 0; $i -lt 20 -and $echoH -eq [IntPtr]::Zero; $i++) {
    Start-Sleep -Milliseconds 500
    foreach ($h in [Win32U]::OfPid($uiPid)) {
        if ([Win32U]::IsWindowVisible($h) -and (WindowTitle $h) -eq 'Echo') { $echoH = $h; break }
    }
}
if ($echoH -eq [IntPtr]::Zero) { throw 'FAIL open: plugin page window "Echo" did not appear' }
Write-Output 'PASS window: Echo page window opened'

# -- 5. WebView2: click send -> spark.rpc -> lazy spawn
$echoEl = $auto::FromHandle($echoH)
$sendBtn = $null
for ($i = 0; $i -lt 20 -and -not $sendBtn; $i++) {
    Start-Sleep -Milliseconds 500
    $sendBtn = $echoEl.FindFirst([System.Windows.Automation.TreeScope]::Descendants, (New-Object System.Windows.Automation.AndCondition((New-Object System.Windows.Automation.PropertyCondition($auto::NameProperty, $sSend)), $btnType)))
    if (-not $sendBtn) { $sendBtn = Find-ButtonByText $echoEl $sSend }
}
if (-not $sendBtn) { throw 'FAIL webview: send button not exposed via UIA (page not loaded?)' }
($sendBtn.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern)).Invoke()

$exe = $null
for ($i = 0; $i -lt 24 -and -not $exe; $i++) {
    Start-Sleep -Milliseconds 500
    $exe = Get-Process -Name 'spark-plugin-echo' -ErrorAction SilentlyContinue
}
if (-not $exe) { throw 'FAIL rpc: spark-plugin-echo.exe not spawned after page rpc (bridge broken?)' }
Write-Output "PASS rpc: page spark.rpc round-trip, exe spawned (pid=$($exe.Id))"
Start-Sleep -Milliseconds 2500
if (-not (Get-Process -Name 'spark-plugin-echo' -ErrorAction SilentlyContinue)) {
    throw 'FAIL rpc: exe died right after spawn (handshake failed?)'
}
Write-Output 'PASS rpc: exe stable after handshake'

# -- 6. close page window -> host.plugin.page_closed -> graceful exit
$closed = $false
try { ($echoEl.GetCurrentPattern([System.Windows.Automation.WindowPattern]::Pattern)).Close(); $closed = $true } catch {}
if (-not $closed) { [Win32U]::PostMessage($echoH, 0x0010, [IntPtr]::Zero, [IntPtr]::Zero) | Out-Null }
$gone = $false
for ($i = 0; $i -lt 16 -and -not $gone; $i++) {
    Start-Sleep -Milliseconds 500
    if (-not (Get-Process -Name 'spark-plugin-echo' -ErrorAction SilentlyContinue)) { $gone = $true }
}
if (-not $gone) { throw 'FAIL close: exe still running after page window closed' }
Write-Output 'PASS close: window closed -> page_closed -> exe exited'

Write-Output 'UI_E2E_ALL_PASS'