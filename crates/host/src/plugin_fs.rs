//! `spark.fs.read` / `spark.fs.write` 的 host 实现（插件开发规范 §7/§8.3）。
//!
//! 分层（纪律同 net_fetch）：`parse_request` 是纯函数校验（ipc 锁内调用，零 IO）；
//! `execute` 是真实文件 IO（ipc_server **放锁后**调用——本地磁盘/网络盘的 IO
//! 可能阻塞数十秒，绝不持 host 锁）。
//!
//! 范围语义（规范 §7："高危权限授权时指定范围"）：`fs.read`/`fs.write` 授权
//! 挂在具体目录上（`plugins-state.json` 的 `fs_scopes`，UI 授权时用目录选择器
//! 指定）。调用路径先做**词法规范化**（解析 `..`/`.`，拒绝逃出根），再对
//! 存在的目标 `canonicalize`（解析符号链接/junction 到真实路径），两者都必须
//! 落在某条授权目录内——越界抛 `PERMISSION_SCOPE`。写入目标若本身是符号
//! 链接同样拒绝（防"范围内软链指向范围外"的写入穿透）。

use anyhow::{anyhow, bail, Result};

/// 单文件读取/单次写入的文本上限（与 net 响应体同量级）。
pub const MAX_TEXT_BYTES: usize = 10 * 1024 * 1024;

const MAX_PATH_CHARS: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsOp {
    Read,
    Write,
}

/// 锁内准备完成、锁外执行的 fs 请求（纯数据，可 Debug/Clone 供测试断言）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginFsRequest {
    pub op: FsOp,
    pub path: String,
    /// write 的文本内容（read 恒为 None）。
    pub text: Option<String>,
    /// 授权目录范围快照（canonicalize 前的原始条目；canonicalize 在锁外做）。
    pub scopes: Vec<String>,
}

/// 从 `plugin_api` 的 args 解析请求：read `{ path }` / write `{ path, text }`。
/// 纯校验（零 IO）：路径形状（绝对性在 execute 阶段以 canonicalize 复核）与
/// 文本上限在这里把关；权限（声明+授权）由 `plugin_api` 分支先行校验。
pub fn parse_request(
    op: FsOp,
    args: &serde_json::Value,
    scopes: Vec<String>,
) -> Result<PluginFsRequest> {
    let obj = args
        .as_object()
        .ok_or_else(|| anyhow!("INVALID_ARGS: fs 需要 object args"))?;
    let path = obj
        .get("path")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow!("INVALID_ARGS: path 必须为非空字符串"))?;
    validate_path(path)?;

    let text = match op {
        FsOp::Read => None,
        FsOp::Write => {
            let t = obj
                .get("text")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("INVALID_ARGS: fs.write 需要 text 字符串"))?;
            if t.len() > MAX_TEXT_BYTES {
                bail!(
                    "FILE_TOO_LARGE: 写入内容超过 {} MB 上限",
                    MAX_TEXT_BYTES / 1024 / 1024
                );
            }
            Some(t.to_string())
        }
    };

    Ok(PluginFsRequest {
        op,
        path: path.to_string(),
        text,
        scopes,
    })
}

fn validate_path(path: &str) -> Result<()> {
    if path.chars().count() > MAX_PATH_CHARS {
        bail!("INVALID_ARGS: path 超过 {MAX_PATH_CHARS} 字符上限");
    }
    if path.chars().any(|c| (c as u32) < 0x20 || c == '\u{7F}') {
        bail!("INVALID_ARGS: path 含非法控制字符");
    }
    if !std::path::Path::new(path).is_absolute() {
        bail!("INVALID_ARGS: path 必须为绝对路径");
    }
    Ok(())
}

/// 锁外执行：canonicalize 校验范围 → 读/写 → 回包。
pub fn execute(req: &PluginFsRequest) -> Result<serde_json::Value> {
    validate_path(&req.path)?;
    let canon_scopes = canonical_scopes(&req.scopes);
    if canon_scopes.is_empty() {
        bail!("PERMISSION_SCOPE: 未配置授权目录范围（请在设置-插件中为该权限添加目录范围）");
    }

    let target = std::path::Path::new(&req.path);
    match req.op {
        FsOp::Read => {
            // canonicalize 解析符号链接/junction 到真实路径——范围内软链指向
            // 范围外文件会在这一步暴露为越界。
            let canon = std::fs::canonicalize(target)
                .map_err(|e| anyhow!("INVALID_ARGS: 文件不存在或不可访问: {} ({e})", req.path))?;
            require_in_scope(&canon, &canon_scopes, &req.path)?;
            let meta = std::fs::metadata(&canon)?;
            if !meta.is_file() {
                bail!("INVALID_ARGS: 目标不是普通文件: {}", canon.display());
            }
            if meta.len() as usize > MAX_TEXT_BYTES {
                bail!(
                    "FILE_TOO_LARGE: 文件超过 {} MB 上限",
                    MAX_TEXT_BYTES / 1024 / 1024
                );
            }
            let text = std::fs::read_to_string(&canon)
                .map_err(|e| anyhow!("UNAVAILABLE: 读取失败: {e}"))?;
            Ok(serde_json::json!({ "text": text }))
        }
        FsOp::Write => {
            // 目标已存在：先按真实路径复检（符号链接可能指向范围外）。
            if let Ok(meta) = std::fs::symlink_metadata(target) {
                if meta.is_symlink() {
                    bail!("PERMISSION_SCOPE: 目标是符号链接，拒绝写入");
                }
                let canon = std::fs::canonicalize(target)
                    .map_err(|e| anyhow!("INVALID_ARGS: 目标不可访问: {} ({e})", req.path))?;
                if !canon.is_file() && canon.is_dir() {
                    bail!("INVALID_ARGS: 目标是目录: {}", canon.display());
                }
                require_in_scope(&canon, &canon_scopes, &req.path)?;
            }
            // 目标可能不存在：词法规范化后按"将落盘的最终路径"校验范围，再
            // 对**最深已存在祖先**做真实路径复检（canonicalize 解析中间
            // junction/symlink 的真实指向——词法校验只看名字形状，范围目录内
            // 一层指向范围外的目录联接会把 `fs::write` 穿透到范围外）。
            let final_path = normalize_lexical(target)?;
            require_lexical_in_scope(&final_path, &canon_scopes, &req.path)?;
            let deepest = deepest_existing_ancestor(&final_path)?;
            require_in_scope(&deepest, &canon_scopes, &req.path)?;
            let bytes = req.text.as_deref().unwrap_or("").as_bytes();
            if bytes.len() > MAX_TEXT_BYTES {
                bail!(
                    "FILE_TOO_LARGE: 写入内容超过 {} MB 上限",
                    MAX_TEXT_BYTES / 1024 / 1024
                );
            }
            if let Some(parent) = final_path.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| anyhow!("UNAVAILABLE: 创建父目录失败: {e}"))?;
                // 落盘前对父目录复验真实路径（create 后 canonicalize 必然可解析；
                // 双重校验把静态绕过面收到零）。残余 TOCTOU（检查与写之间被并发
                // 进程换接）需要本机竞争者——按整改清单 V1 的诚实定位（能改本机
                // 文件的程序已有代码执行能力），该窗口为可接受残余，此处不引入
                // 不可重入的额外锁。
                let canon_parent = std::fs::canonicalize(parent)
                    .map_err(|e| anyhow!("INVALID_ARGS: 父目录不可访问 ({e})"))?;
                require_in_scope(&strip_verbatim(&canon_parent), &canon_scopes, &req.path)?;
            }
            // 最终文件组件也不能是（检查后新出现的）符号链接。
            if let Ok(meta) = std::fs::symlink_metadata(&final_path) {
                if meta.is_symlink() {
                    bail!("PERMISSION_SCOPE: 目标是符号链接，拒绝写入");
                }
            }
            std::fs::write(&final_path, bytes)
                .map_err(|e| anyhow!("UNAVAILABLE: 写入失败: {e}"))?;
            Ok(serde_json::json!({ "ok": true }))
        }
    }
}

/// 目标路径中**最深且真实存在**的祖先并 canonicalize（解析中间 junction /
/// symlink 的真实指向）。绝对路径至少盘根存在，循环必然命中。
fn deepest_existing_ancestor(p: &std::path::Path) -> Result<std::path::PathBuf> {
    let mut probe = p.to_path_buf();
    loop {
        match std::fs::canonicalize(&probe) {
            Ok(c) => return Ok(strip_verbatim(&c)),
            Err(_) => match probe.parent() {
                Some(parent) => probe = parent.to_path_buf(),
                None => bail!("INVALID_ARGS: path 无法解析出存在的祖先"),
            },
        }
    }
}

/// 规范化授权目录（解析符号链接/相对段）；失效条目跳过——全失效时
/// execute 报"未配置授权目录范围"，与"授权了但都不可用"同语义。
fn canonical_scopes(scopes: &[String]) -> Vec<std::path::PathBuf> {
    scopes
        .iter()
        .filter_map(|s| std::fs::canonicalize(s).ok())
        .filter(|p| p.is_dir())
        .map(|p| strip_verbatim(&p))
        .collect()
}

/// Windows canonicalize 返回 `\\?\` verbatim 路径：比较与展示统一剥掉前缀
/// （`\\?\UNC\server\share` 还原为 `\\server\share`）。剥掉后仍是合法绝对
/// 路径（普通路径 API 可直接使用），且与词法规范化路径可比。
fn strip_verbatim(p: &std::path::Path) -> std::path::PathBuf {
    let s = p.to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        return std::path::PathBuf::from(format!(r"\\{rest}"));
    }
    if let Some(rest) = s.strip_prefix(r"\\?\") {
        return std::path::PathBuf::from(rest);
    }
    p.to_path_buf()
}

/// 词法规范化（不要求存在）：绝对化 + 解析 `.`/`..`（`..` 逃出根即拒绝）。
/// Windows 路径组件已由 `Components` 逐段给出，反斜杠/大小写差异由后置的
/// 前缀包含比较统一处理。
fn normalize_lexical(p: &std::path::Path) -> Result<std::path::PathBuf> {
    use std::path::Component;
    let abs = std::path::absolute(p).map_err(|e| anyhow!("INVALID_ARGS: path 无法绝对化: {e}"))?;
    let mut stack: Vec<std::ffi::OsString> = Vec::new();
    for comp in abs.components() {
        use std::path::Component;
        match comp {
            Component::Normal(c) => stack.push(c.to_os_string()),
            Component::ParentDir => {
                if stack.pop().is_none() {
                    bail!("INVALID_ARGS: path 越界（.. 超出根）");
                }
            }
            Component::CurDir | Component::RootDir | Component::Prefix(_) => {}
        }
    }
    if stack.is_empty() {
        bail!("INVALID_ARGS: path 缺少文件名");
    }
    // 从原始绝对路径的根开始拼回（保留盘符/UNC 前缀）。
    let mut out = std::path::PathBuf::new();
    for comp in abs.components() {
        match comp {
            Component::Prefix(_) | Component::RootDir => out.push(comp.as_os_str()),
            _ => break,
        }
    }
    for c in &stack {
        out.push(c);
    }
    Ok(out)
}

/// 目标（已 canonicalize）必须落在某条授权目录内（含边界：目录本身也算在内，
/// 但 read 分支另有 is_file 把关）。比较统一转小写（Windows 大小写不敏感）。
fn require_in_scope(
    canon: &std::path::Path,
    scopes: &[std::path::PathBuf],
    display: &str,
) -> Result<()> {
    let cand = strip_verbatim(canon).to_string_lossy().into_owned();
    in_scope_cmp(&cand, scopes, display)
}

/// 写入前对"将落盘的词法路径"做同一套范围校验（目标可能尚不存在、无法
/// canonicalize——按词法形态校验，与存在目标的 canonicalize 校验叠加后，
/// 真实读写路径必然在范围内）。
fn require_lexical_in_scope(
    lex: &std::path::Path,
    scopes: &[std::path::PathBuf],
    display: &str,
) -> Result<()> {
    let cand = lex.to_string_lossy().into_owned();
    in_scope_cmp(&cand, scopes, display)
}

/// 前缀包含比较（大小写不敏感，双方剥尾随反斜杠——盘根范围 `D:\` 归一为
/// `d:` 后 `d:\x.txt` 以 `d:\` 为界命中，不产生盘根范围恒拒的伪拒绝）。
fn in_scope_cmp(cand: &str, scopes: &[std::path::PathBuf], display: &str) -> Result<()> {
    let cand = cand.to_ascii_lowercase();
    let cand = cand.trim_end_matches('\\');
    for scope in scopes {
        let s = scope.to_string_lossy().to_ascii_lowercase();
        let s = s.trim_end_matches('\\');
        if cand == s || cand.starts_with(&format!("{s}\\")) {
            return Ok(());
        }
    }
    bail!("PERMISSION_SCOPE: 路径超出授权目录范围: {display}");
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tmp_root(tag: &str) -> std::path::PathBuf {
        // cargo test 并行跑同 crate 内测试：目录按用例名隔离，避免互删。
        let dir =
            std::env::temp_dir().join(format!("spark_fs_test_{}_{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_req(path: &str, text: &str, scopes: &[&str]) -> PluginFsRequest {
        parse_request(
            FsOp::Write,
            &json!({ "path": path, "text": text }),
            scopes.iter().map(|s| s.to_string()).collect(),
        )
        .unwrap()
    }

    fn read_req(path: &str, scopes: &[&str]) -> PluginFsRequest {
        parse_request(
            FsOp::Read,
            &json!({ "path": path }),
            scopes.iter().map(|s| s.to_string()).collect(),
        )
        .unwrap()
    }

    #[test]
    fn parse_rejects_relative_and_empty_path() {
        let err = parse_request(FsOp::Read, &json!({ "path": "" }), Vec::new()).unwrap_err();
        assert!(err.to_string().contains("INVALID_ARGS"), "{err}");
        let err =
            parse_request(FsOp::Read, &json!({ "path": "rel/x.txt" }), Vec::new()).unwrap_err();
        assert!(err.to_string().contains("INVALID_ARGS"), "{err}");
    }

    #[test]
    fn parse_rejects_write_over_cap() {
        let big = "x".repeat(MAX_TEXT_BYTES + 1);
        let err = parse_request(
            FsOp::Write,
            &json!({ "path": "C:\\x.txt", "text": big }),
            Vec::new(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("FILE_TOO_LARGE"), "{err}");
    }

    #[test]
    fn read_requires_scopes_and_containment() {
        let root = tmp_root("read");
        let dir = root.join("scope");
        std::fs::create_dir_all(&dir).unwrap();
        let inside = dir.join("a.txt");
        std::fs::write(&inside, "hello").unwrap();
        let outside = root.join("b.txt");
        std::fs::write(&outside, "secret").unwrap();
        let scope_s = dir.to_string_lossy().into_owned();

        // 无范围 → PERMISSION_SCOPE。
        let err = execute(&read_req(inside.to_string_lossy().as_ref(), &[])).unwrap_err();
        assert!(err.to_string().contains("PERMISSION_SCOPE"), "{err}");
        // 范围外 → PERMISSION_SCOPE。
        let err = execute(&read_req(
            outside.to_string_lossy().as_ref(),
            &[scope_s.as_str()],
        ))
        .unwrap_err();
        assert!(err.to_string().contains("PERMISSION_SCOPE"), "{err}");
        // 范围内 → 读到内容。
        let out = execute(&read_req(
            inside.to_string_lossy().as_ref(),
            &[scope_s.as_str()],
        ))
        .unwrap();
        assert_eq!(out["text"], "hello");
    }

    #[test]
    fn write_containment_and_new_dirs() {
        let root = tmp_root("write");
        let dir = root.join("scope");
        std::fs::create_dir_all(&dir).unwrap();
        let scope_s = dir.to_string_lossy().into_owned();

        // 范围外写入 → PERMISSION_SCOPE（含 .. 逃逸：词法规范化后越界）。
        let outside = root.join("evil.txt");
        let err = execute(&write_req(
            outside.to_string_lossy().as_ref(),
            "x",
            &[scope_s.as_str()],
        ))
        .unwrap_err();
        assert!(err.to_string().contains("PERMISSION_SCOPE"), "{err}");
        let escape = dir.join("..").join("escape.txt");
        let err = execute(&write_req(
            escape.to_string_lossy().as_ref(),
            "x",
            &[scope_s.as_str()],
        ))
        .unwrap_err();
        assert!(err.to_string().contains("PERMISSION_SCOPE"), "{err}");

        // 范围内新建嵌套目录/文件 → 成功。
        let target = dir.join("sub").join("new.txt");
        let out = execute(&write_req(
            target.to_string_lossy().as_ref(),
            "data",
            &[scope_s.as_str()],
        ))
        .unwrap();
        assert_eq!(out["ok"], true);
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "data");

        // 范围声明带尾随反斜杠（盘根形态 `D:\` 归一）→ 同样命中。
        let trailing = format!("{}\\", scope_s);
        let out = execute(&read_req(
            target.to_string_lossy().as_ref(),
            &[trailing.as_str()],
        ))
        .unwrap();
        assert_eq!(out["text"], "data");

        let _ = std::fs::remove_dir_all(root);
    }

    /// 用 `mklink /J` 建目录联接（junction，无需管理员）。失败返回 None
    /// （极老环境/沙箱可能拒绝，此时跳过该形态断言）。
    fn make_junction(link: &std::path::Path, target: &std::path::Path) -> bool {
        use std::os::windows::process::CommandExt;
        std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    #[test]
    fn write_and_read_do_not_traverse_junction_out_of_scope() {
        // 🔴#1 回归：范围内一层指向范围外的目录联接——读（canonicalize 解析
        // 真实路径）与写（最深已存在祖先 + 落盘前父目录复验）都必须拒绝。
        let root = tmp_root("junction");
        let scope = root.join("scope");
        let outside = root.join("outside");
        std::fs::create_dir_all(&scope).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret.txt"), "outside").unwrap();
        let link = scope.join("link");
        if !make_junction(&link, &outside) {
            return; // 环境不支持 junction：跳过该形态
        }
        let scope_s = scope.to_string_lossy().into_owned();

        // 经联接读范围外文件 → 拒（read 分支 canonicalize 解析真实路径）。
        let err = execute(&read_req(
            link.join("secret.txt").to_string_lossy().as_ref(),
            &[scope_s.as_str()],
        ))
        .unwrap_err();
        assert!(err.to_string().contains("PERMISSION_SCOPE"), "{err}");

        // 经联接写新文件（目标不存在，词法校验本会放行）→ 真实路径校验拒绝。
        let err = execute(&write_req(
            link.join("pwn.txt").to_string_lossy().as_ref(),
            "x",
            &[scope_s.as_str()],
        ))
        .unwrap_err();
        assert!(err.to_string().contains("PERMISSION_SCOPE"), "{err}");
        assert!(
            !outside.join("pwn.txt").exists(),
            "junction 写入不应落盘到范围外"
        );

        // 对照：范围外目录本身作为范围 → 经联接可正常访问（范围声明即用户意图）。
        let outside_scope = outside.to_string_lossy().into_owned();
        let out = execute(&read_req(
            link.join("secret.txt").to_string_lossy().as_ref(),
            &[outside_scope.as_str()],
        ))
        .unwrap();
        assert_eq!(out["text"], "outside");

        let _ = std::fs::remove_dir_all(root);
    }
}
