//! 内置系统命令执行层（Win32）。
//!
//! 与 `spark_index::builtin` 的命令表一一对应；确认文案/别名等在 index 侧，
//! 这里只负责真正干活。返回 `BuiltinOutcome` 由 app.rs 映射为 InvokeResult。

use crate::shell;
use anyhow::{anyhow, bail, Context, Result};
use std::io::{Read, Write};
use std::net::{Ipv4Addr, TcpStream};
use std::time::Duration;
use windows::Win32::Foundation::{GetLastError, ERROR_BUFFER_OVERFLOW, HANDLE, LUID, NO_ERROR};
use windows::Win32::NetworkManagement::IpHelper::{
    GetAdaptersAddresses, GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_DNS_SERVER, GAA_FLAG_SKIP_MULTICAST,
    IP_ADAPTER_ADDRESSES_LH,
};
use windows::Win32::Networking::WinSock::{AF_INET, SOCKADDR_IN};
use windows::Win32::Security::{
    AdjustTokenPrivileges, LookupPrivilegeValueW, LUID_AND_ATTRIBUTES, SE_PRIVILEGE_ENABLED,
    SE_SHUTDOWN_NAME, TOKEN_ADJUST_PRIVILEGES, TOKEN_PRIVILEGES, TOKEN_QUERY,
};
use windows::Win32::System::Power::SetSuspendState;
use windows::Win32::System::Shutdown::{
    ExitWindowsEx, LockWorkStation, EWX_LOGOFF, EWX_POWEROFF, EWX_REBOOT, EXIT_WINDOWS_FLAGS,
    SHUTDOWN_REASON,
};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows::Win32::UI::Shell::{SHEmptyRecycleBinW, SHERB_NOCONFIRMATION, SHERB_NOPROGRESSUI};

/// 执行结果：要么关闭并提示，要么复制文本（如内网IP）。
pub enum BuiltinOutcome {
    Close(String),
    CopyText(String),
}

/// 执行一条内置命令。`id` 与 `spark_index::builtin::BuiltinSpec::id` 对应。
pub fn execute(id: &str) -> Result<BuiltinOutcome> {
    match id {
        "builtin.lock" => lock(),
        "builtin.shutdown" => power(EWX_POWEROFF, "已关机"),
        "builtin.reboot" => power(EWX_REBOOT, "正在重启"),
        "builtin.logoff" => power(EWX_LOGOFF, "正在注销"),
        "builtin.sleep" => sleep(),
        "builtin.empty_recycle_bin" => empty_recycle_bin(),
        "builtin.recycle_bin" => {
            shell::shell_open("shell:RecycleBinFolder").context("打开回收站失败")?;
            Ok(BuiltinOutcome::Close("已打开回收站".into()))
        }
        "builtin.screenshot" => screenshot(),
        "builtin.settings" => {
            shell::shell_open("ms-settings:").context("打开 Windows 设置失败")?;
            Ok(BuiltinOutcome::Close("已打开 Windows 设置".into()))
        }
        "builtin.explorer" => {
            shell::shell_open("explorer.exe").context("打开文件资源管理器失败")?;
            Ok(BuiltinOutcome::Close("已打开文件资源管理器".into()))
        }
        "builtin.remote_desktop" => {
            shell::shell_open("mstsc.exe").context("打开远程桌面连接失败")?;
            Ok(BuiltinOutcome::Close("已打开远程桌面连接".into()))
        }
        // 环境变量对话框：rundll32 带参数（system properties 的高级页子对话框）
        "builtin.env_vars" => {
            shell::shell_open_with_args("rundll32.exe", "sysdm.cpl,EditEnvironmentVariables")
                .context("打开环境变量失败")?;
            Ok(BuiltinOutcome::Close("已打开环境变量".into()))
        }
        "builtin.lan_ip" => {
            let ip = lan_ip().context("获取内网 IP 失败")?;
            Ok(BuiltinOutcome::CopyText(ip))
        }
        "builtin.public_ip" => {
            let ip = public_ip().context("获取公网 IP 失败")?;
            Ok(BuiltinOutcome::CopyText(ip))
        }
        // 其余打开类系统命令（注册表/设备管理器/计算器等）：统一 ShellExecuteW
        _ => {
            if let Some((_, target, msg)) = OPEN_APPS.iter().find(|(cmd, _, _)| *cmd == id) {
                shell::shell_open(target).with_context(|| format!("打开 {msg} 失败"))?;
                return Ok(BuiltinOutcome::Close(msg.to_string()));
            }
            bail!("unknown builtin: {id}")
        }
    }
}

/// 打开类命令表：(id, 目标, 完成提示)。均无确认、无特殊逻辑。
const OPEN_APPS: &[(&str, &str, &str)] = &[
    (
        "builtin.regedit",
        r"C:\Windows\regedit.exe", // Win11 起位于系统根目录
        "已打开注册表编辑器",
    ),
    (
        "builtin.msinfo",
        r"C:\Windows\System32\msinfo32.exe",
        "已打开系统信息",
    ),
    (
        "builtin.sysprops",
        r"C:\Windows\System32\sysdm.cpl",
        "已打开系统属性",
    ),
    (
        "builtin.device_manager",
        r"C:\Windows\System32\devmgmt.msc",
        "已打开设备管理器",
    ),
    (
        "builtin.disk_management",
        r"C:\Windows\System32\diskmgmt.msc",
        "已打开磁盘管理",
    ),
    (
        "builtin.computer_management",
        r"C:\Windows\System32\compmgmt.msc",
        "已打开计算机管理",
    ),
    (
        "builtin.services",
        r"C:\Windows\System32\services.msc",
        "已打开服务",
    ),
    (
        "builtin.event_viewer",
        r"C:\Windows\System32\eventvwr.msc",
        "已打开事件查看器",
    ),
    (
        "builtin.task_manager",
        r"C:\Windows\System32\Taskmgr.exe",
        "已打开任务管理器",
    ),
    (
        "builtin.task_scheduler",
        r"C:\Windows\System32\taskschd.msc",
        "已打开任务计划程序",
    ),
    (
        "builtin.resource_monitor",
        r"C:\Windows\System32\resmon.exe",
        "已打开资源监视器",
    ),
    (
        "builtin.performance_monitor",
        r"C:\Windows\System32\perfmon.msc",
        "已打开性能监视器",
    ),
    (
        "builtin.secpol",
        r"C:\Windows\System32\secpol.msc",
        "已打开本地安全策略",
    ),
    (
        "builtin.gpedit",
        r"C:\Windows\System32\gpedit.msc",
        "已打开组策略编辑器",
    ),
    (
        "builtin.msconfig",
        r"C:\Windows\System32\MSConfig.exe",
        "已打开系统配置",
    ),
    (
        "builtin.shared_folders",
        r"C:\Windows\System32\fsmgmt.msc",
        "已打开共享文件夹",
    ),
    (
        "builtin.users_groups",
        r"C:\Windows\System32\lusrmgr.msc",
        "已打开本地用户和组",
    ),
    (
        "builtin.control_panel",
        r"C:\Windows\System32\control.exe",
        "已打开控制面板",
    ),
    (
        "builtin.programs_features",
        r"C:\Windows\System32\appwiz.cpl",
        "已打开程序和功能",
    ),
    (
        "builtin.network_connections",
        r"C:\Windows\System32\ncpa.cpl",
        "已打开网络连接",
    ),
    (
        "builtin.sound",
        r"C:\Windows\System32\mmsys.cpl",
        "已打开声音设置",
    ),
    (
        "builtin.power_options",
        r"C:\Windows\System32\powercfg.cpl",
        "已打开电源选项",
    ),
    (
        "builtin.display_settings",
        "ms-settings:display",
        "已打开显示设置",
    ),
    (
        "builtin.date_time",
        r"C:\Windows\System32\timedate.cpl",
        "已打开日期和时间",
    ),
    (
        "builtin.mouse",
        r"C:\Windows\System32\main.cpl",
        "已打开鼠标设置",
    ),
    (
        "builtin.region",
        r"C:\Windows\System32\intl.cpl",
        "已打开区域设置",
    ),
    ("builtin.fonts", "shell:fonts", "已打开字体"),
    (
        "builtin.cmd",
        r"C:\Windows\System32\cmd.exe",
        "已打开命令提示符",
    ),
    (
        "builtin.powershell",
        r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe",
        "已打开 Windows PowerShell",
    ),
    (
        "builtin.calc",
        r"C:\Windows\System32\calc.exe",
        "已打开计算器",
    ),
    (
        "builtin.notepad",
        r"C:\Windows\System32\notepad.exe",
        "已打开记事本",
    ),
    (
        "builtin.paint",
        r"C:\Windows\System32\mspaint.exe",
        "已打开画图",
    ),
    (
        "builtin.magnifier",
        r"C:\Windows\System32\Magnify.exe",
        "已打开放大镜",
    ),
    (
        "builtin.on_screen_keyboard",
        r"C:\Windows\System32\osk.exe",
        "已打开屏幕键盘",
    ),
];

/// 锁屏：`LockWorkStation` 立即锁定当前会话。
fn lock() -> Result<BuiltinOutcome> {
    unsafe { LockWorkStation() }.context("锁屏失败")?;
    Ok(BuiltinOutcome::Close("已锁定".into()))
}

/// 关机/重启/注销：先启用 SeShutdownPrivilege，再 ExitWindowsEx。
fn power(flag: EXIT_WINDOWS_FLAGS, message: &'static str) -> Result<BuiltinOutcome> {
    enable_shutdown_privilege().context("启用关机权限失败")?;
    unsafe { ExitWindowsEx(flag, SHUTDOWN_REASON(0)) }.context("执行系统电源操作失败")?;
    Ok(BuiltinOutcome::Close(message.into()))
}

/// 睡眠：立即挂起（不写入休眠文件）。
fn sleep() -> Result<BuiltinOutcome> {
    let ok = unsafe { SetSuspendState(false, true, false) };
    if !ok {
        bail!("SetSuspendState failed");
    }
    Ok(BuiltinOutcome::Close("已进入睡眠".into()))
}

/// 清空回收站：所有驱动器，无确认弹窗、无进度条（确认在 UI 层完成）。
fn empty_recycle_bin() -> Result<BuiltinOutcome> {
    unsafe { SHEmptyRecycleBinW(None, None, SHERB_NOCONFIRMATION | SHERB_NOPROGRESSUI) }
        .context("清空回收站失败")?;
    Ok(BuiltinOutcome::Close("回收站已清空".into()))
}

/// 截图：优先 `ms-screenclip:`（Win11 截图工具），失败回退 snippingtool.exe。
fn screenshot() -> Result<BuiltinOutcome> {
    if shell::shell_open("ms-screenclip:").is_err() {
        shell::shell_open("snippingtool.exe").context("打开截图工具失败")?;
    }
    Ok(BuiltinOutcome::Close("已打开截图工具".into()))
}

/// 内网 IP：取第一个非回环、非链路本地的 IPv4 地址。
fn lan_ip() -> Result<String> {
    let flags = GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_SKIP_DNS_SERVER;

    // 缓冲区大小可能不够，按 GetAdaptersAddresses 返回的 size 重试
    let mut size: u32 = 0;
    let mut buf: Vec<u8> = Vec::new();
    loop {
        let rc = unsafe {
            GetAdaptersAddresses(
                AF_INET.0 as u32,
                flags,
                None,
                if buf.is_empty() {
                    None
                } else {
                    Some(buf.as_mut_ptr() as *mut IP_ADAPTER_ADDRESSES_LH)
                },
                &mut size,
            )
        };
        match rc {
            rc if rc == NO_ERROR.0 => break,
            rc if rc == ERROR_BUFFER_OVERFLOW.0 => {
                buf.resize(size as usize, 0);
                continue;
            }
            rc => bail!("GetAdaptersAddresses failed: {rc}"),
        }
    }
    if buf.is_empty() {
        bail!("no adapters");
    }

    // 优先私有网段（10.x / 172.16-31.x / 192.168.x —— "内网"语义），
    // 没有私有地址再退回任意非回环 IPv4（如直连公网/VPN 的机器）。
    let mut first: Option<String> = None;
    let mut cur = buf.as_ptr() as *const IP_ADAPTER_ADDRESSES_LH;
    while !cur.is_null() {
        let adapter = unsafe { &*cur };
        // 跳过回环适配器（IF_TYPE_SOFTWARE_LOOPBACK = 24）
        if adapter.IfType != 24 {
            if let Some(ip) = first_usable_ipv4(adapter) {
                if is_private_ipv4(&ip) {
                    return Ok(ip);
                }
                if first.is_none() {
                    first = Some(ip);
                }
            }
        }
        cur = adapter.Next;
    }
    first.ok_or_else(|| anyhow!("no usable LAN IPv4 address"))
}

/// 是否为私有网段地址（RFC 1918）。
fn is_private_ipv4(ip: &str) -> bool {
    let parts: Vec<u8> = ip.split('.').filter_map(|s| s.parse().ok()).collect();
    if parts.len() != 4 {
        return false;
    }
    let (a, b) = (parts[0], parts[1]);
    a == 10 || (a == 172 && (16..=31).contains(&b)) || (a == 192 && b == 168)
}

/// 公网出口 IP：逐个尝试明文 HTTP IP 服务，取第一个成功的（utools 外网IP 同思路）。
fn public_ip() -> Result<String> {
    // 按可用性排序；全部失败才报错
    for host in ["ip.3322.net", "myip.ipip.net"] {
        match fetch_http_ip(host) {
            Ok(ip) => return Ok(ip),
            Err(e) => tracing::warn!(?e, host, "public IP service failed"),
        }
    }
    bail!("所有公网 IP 服务均不可达")
}

/// 对 `host:80` 发一个最简单的 HTTP/1.1 GET，从响应体中提取 IPv4。
fn fetch_http_ip(host: &str) -> Result<String> {
    let mut stream = TcpStream::connect((host, 80)).context("connect")?;
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .context("set read timeout")?;
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .context("set write timeout")?;
    let req = format!(
        "GET / HTTP/1.1\r\nHost: {host}\r\nUser-Agent: Spark-Launcher/0.1\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(req.as_bytes()).context("write request")?;
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).context("read response")?;
    let text = String::from_utf8_lossy(&buf);
    let body = text.split("\r\n\r\n").nth(1).unwrap_or("");
    extract_ipv4(body).ok_or_else(|| anyhow!("响应中未找到 IPv4 地址"))
}

/// 从文本中提取第一个 x.x.x.x（每段 0-255）的 IPv4。
fn extract_ipv4(text: &str) -> Option<String> {
    for token in text.split(|c: char| !c.is_ascii_digit() && c != '.') {
        let parts: Vec<&str> = token.split('.').collect();
        if parts.len() != 4 {
            continue;
        }
        let octets: Option<Vec<u8>> = parts.iter().map(|p| p.parse().ok()).collect();
        if let Some(octets) = octets {
            return Some(
                octets
                    .iter()
                    .map(|o| o.to_string())
                    .collect::<Vec<_>>()
                    .join("."),
            );
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_ipv4_from_plain_text() {
        assert_eq!(
            extract_ipv4("112.66.41.32"),
            Some("112.66.41.32".to_string())
        );
        assert_eq!(
            extract_ipv4("当前 IP：112.66.41.32  来自于：中国"),
            Some("112.66.41.32".to_string())
        );
        assert_eq!(extract_ipv4("no ip here"), None);
        assert_eq!(extract_ipv4("999.1.2.3"), None, "越界段不算 IP");
    }

    #[test]
    fn private_ranges_detected() {
        assert!(is_private_ipv4("10.0.0.1"));
        assert!(is_private_ipv4("172.16.5.5"));
        assert!(is_private_ipv4("192.168.1.1"));
        assert!(!is_private_ipv4("172.32.0.1"));
        assert!(!is_private_ipv4("27.183.254.169"));
    }

    #[test]
    fn network_order_conversion() {
        // 192.168.1.66 网络序 0xC0A80142，LE 机器上 S_addr 读作 0x4201A8C0
        assert_eq!(
            ipv4_from_network_order(0x4201A8C0).to_string(),
            "192.168.1.66"
        );
        // 169.254.183.27（链路本地）反转后也不该被误判成公网段
        assert_eq!(
            ipv4_from_network_order(0x1BB7FEA9).to_string(),
            "169.254.183.27"
        );
    }
}

/// 网络字节序（大端）的 IPv4 数值 → 点分字符串。
/// `S_addr` 在内存里按网络序存储，LE 机器上直接读是反的：
/// 192.168.1.66 存为 0xC0A80142，LE 读作 0x4201A8C0，需 from_be 还原。
fn ipv4_from_network_order(raw: u32) -> Ipv4Addr {
    Ipv4Addr::from(u32::from_be(raw))
}

/// 遍历单播地址，返回第一个可用的 IPv4（排除回环 127.* 与链路本地 169.254.*）。
fn first_usable_ipv4(adapter: &IP_ADAPTER_ADDRESSES_LH) -> Option<String> {
    let mut uni = adapter.FirstUnicastAddress;
    while !uni.is_null() {
        let addr = unsafe { &*uni };
        let sockaddr = addr.Address.lpSockaddr;
        if !sockaddr.is_null() {
            let sin = unsafe { &*(sockaddr as *const SOCKADDR_IN) };
            if sin.sin_family == AF_INET {
                // S_addr 是网络字节序（大端），转回点分要按主机序还原数值
                let ip = ipv4_from_network_order(unsafe { sin.sin_addr.S_un.S_addr });
                let octets = ip.octets();
                let is_usable = octets[0] != 127
                    && !(octets[0] == 169 && octets[1] == 254)
                    && octets.iter().any(|b| *b != 0);
                if is_usable {
                    return Some(ip.to_string());
                }
            }
        }
        uni = addr.Next;
    }
    None
}

/// 启用 SeShutdownPrivilege（ExitWindowsEx 关机/重启需要）。
fn enable_shutdown_privilege() -> Result<()> {
    unsafe {
        let mut token: HANDLE = HANDLE::default();
        OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY,
            &mut token,
        )?;
        let mut luid = LUID::default();
        LookupPrivilegeValueW(None, SE_SHUTDOWN_NAME, &mut luid)?;
        let tp = TOKEN_PRIVILEGES {
            PrivilegeCount: 1,
            Privileges: [LUID_AND_ATTRIBUTES {
                Luid: luid,
                Attributes: SE_PRIVILEGE_ENABLED,
            }],
        };
        AdjustTokenPrivileges(token, false, Some(&tp), 0, None, None)?;
        // AdjustTokenPrivileges 可能静默失败（部分特权未生效），显式检查
        let last = GetLastError();
        if last.0 != 0 {
            bail!("AdjustTokenPrivileges failed: {last:?}");
        }
    }
    Ok(())
}
