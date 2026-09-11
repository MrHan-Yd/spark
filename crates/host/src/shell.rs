//! Launch apps / reveal in Explorer via ShellExecuteW.

use anyhow::{bail, Context, Result};
use std::path::Path;
use tracing::debug;
use windows::core::PCWSTR;
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

/// Open a file, shortcut, or executable with the default verb.
pub fn shell_open(target: &str) -> Result<()> {
    shell_execute(target, "open")
}

/// `spark.shell.openExternal`（《插件开发规范》§8.3）：用系统默认程序打开 URL/文件。
///
/// 插件桥入口的输入净化：拒绝空串/超长/任何 C0 控制字符（含 CR/LF/NUL——
/// ShellExecuteW 收到嵌 NUL 的 wide 串会静默截断，控制字符则可能拼出非预期
/// 命令行）。语义即"系统默认程序打开"，verb 恒为 open，不代执行带参命令。
pub fn open_external(target: &str) -> Result<()> {
    if target.is_empty() {
        bail!("INVALID_ARGS: target 不能为空");
    }
    if target.len() > 2048 {
        bail!("INVALID_ARGS: target 超过 2048 字符上限");
    }
    if target.chars().any(|c| (c as u32) < 0x20 || c == '\u{7F}') {
        bail!("INVALID_ARGS: target 含非法控制字符");
    }
    shell_execute(target, "open")
}

/// Open a file/executable with extra command-line parameters（如 rundll32 环境变量对话框）。
pub fn shell_open_with_args(file: &str, args: &str) -> Result<()> {
    let wide = to_wide(file);
    let params = to_wide(args);
    // ShellExecuteW returns > 32 on success (as HINSTANCE cast).
    let rc = unsafe {
        ShellExecuteW(
            None,
            PCWSTR::null(),
            PCWSTR(wide.as_ptr()),
            PCWSTR(params.as_ptr()),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    let code = rc.0 as isize;
    if code <= 32 {
        bail!("ShellExecute({file} {args}) failed (code {code})");
    }
    Ok(())
}

/// Open elevated（触发 UAC）via the "runas" verb.
pub fn shell_runas(target: &str) -> Result<()> {
    shell_execute(target, "runas")
}

fn shell_execute(target: &str, verb: &str) -> Result<()> {
    if target.is_empty() {
        bail!("empty target");
    }
    let wide = to_wide(target);
    let operation = to_wide(verb);
    // ShellExecuteW returns > 32 on success (as HINSTANCE cast).
    let rc = unsafe {
        ShellExecuteW(
            None,
            PCWSTR(operation.as_ptr()),
            PCWSTR(wide.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    let code = rc.0 as isize;
    if code <= 32 {
        bail!("ShellExecute({verb}) failed for '{target}' (code {code})");
    }
    Ok(())
}

/// Open Explorer with the file selected when possible.
pub fn shell_reveal(target: &str) -> Result<()> {
    let path = Path::new(target);
    if path.is_file() {
        let arg = format!("/select,{}", path.to_string_lossy());
        let explorer = to_wide("explorer.exe");
        let params = to_wide(&arg);
        let rc = unsafe {
            ShellExecuteW(
                None,
                PCWSTR::null(),
                PCWSTR(explorer.as_ptr()),
                PCWSTR(params.as_ptr()),
                PCWSTR::null(),
                SW_SHOWNORMAL,
            )
        };
        let code = rc.0 as isize;
        if code <= 32 {
            bail!("reveal failed (code {code})");
        }
        return Ok(());
    }
    if path.is_dir() {
        return shell_open(target);
    }
    // .lnk or missing: still try open parent
    if let Some(parent) = path.parent() {
        return shell_open(&parent.to_string_lossy());
    }
    shell_open(target)
}

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Invoke default or named action on a candidate target.
pub fn invoke_action(target: Option<&str>, action_id: &str) -> Result<()> {
    let target = target.context("candidate has no target")?;
    match action_id {
        "open" | "" => shell_open(target),
        "runas" => shell_runas(target),
        "reveal" => shell_reveal(target),
        // Secondary shortcut actions (merged .lnk): open their own target
        other => {
            debug!(action = other, "falling back to default open");
            shell_open(target)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wide_null_terminated() {
        let w = to_wide("ab");
        assert_eq!(w[w.len() - 1], 0);
    }
}
