//! spark-sign CLI：Spark 插件签名工具入口（规范 §9.1）。

use anyhow::Result;
use clap::{Parser, Subcommand};
use spark_plugin_manager::SignState;
use spark_sign as libsign;
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "spark-sign",
    about = "Spark 插件签名工具：生成密钥、签名插件、验签自检"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// 生成 Ed25519 密钥对：写私钥文件，打印公钥（base64）。
    Keygen {
        /// 私钥输出路径（base64 seed；勿提交仓库）。
        #[arg(long)]
        out: PathBuf,
        /// 密钥标识，写入 signature.json 的 key_id，须与 host 内置 TRUSTED_KEYS 对应。
        #[arg(long)]
        key_id: String,
    },
    /// 对插件目录签名，写 signature.json。
    Sign {
        /// 插件目录（含 plugin.json）。
        #[arg(long)]
        dir: PathBuf,
        /// 私钥文件路径（base64 seed）。
        #[arg(long)]
        key: PathBuf,
        /// 密钥标识。
        #[arg(long)]
        key_id: String,
        /// 只打印将签的文件清单，不写 signature.json。
        #[arg(long, default_value_t = false)]
        dry_run: bool,
    },
    /// 验签：默认用 host 内置公钥表；--pubkey 指定 base64 公钥自检开发密钥。
    Verify {
        #[arg(long)]
        dir: PathBuf,
        /// 可选：base64 Ed25519 公钥（不传则用内置 TRUSTED_KEYS）。
        #[arg(long)]
        pubkey: Option<String>,
    },
    /// 打印 signature.json 内容与验签结果。
    Inspect {
        #[arg(long)]
        dir: PathBuf,
        #[arg(long)]
        pubkey: Option<String>,
    },
    /// 校验官方仓库 registry.json 准入（3.1）：每个版本必须带有效官方签名字段。
    /// --dir 给仓库根（本地镜像）时，进一步对每个 path 目录做包内全量验签。
    CheckRegistry {
        #[arg(long)]
        registry: PathBuf,
        /// 仓库根目录（含各插件版本子目录）。缺省只做 registry 结构校验。
        #[arg(long)]
        dir: Option<PathBuf>,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Keygen { out, key_id } => {
            let sk = libsign::generate_keypair();
            libsign::write_private_key(&out, &sk)?;
            println!("key_id:      {key_id}");
            println!(
                "private key:  {}  (keep secret, never commit)",
                out.display()
            );
            println!("public key:   {}", libsign::pubkey_base64(&sk));
            println!();
            println!("把上面 public key 填入 crates/plugin-manager/src/signing/keys.rs 的 TRUSTED_KEYS。");
            Ok(())
        }
        Cmd::Sign {
            dir,
            key,
            key_id,
            dry_run,
        } => {
            let out = libsign::sign_dir(&dir, &key, &key_id, dry_run)?;
            println!(
                "{} plugin {} v{} (key_id={}, {} files)",
                if dry_run { "dry-run" } else { "signed" },
                out.plugin_id,
                out.version,
                out.key_id,
                out.files.len()
            );
            for f in &out.files {
                println!("  {}  {}", f.sha256, f.path);
            }
            if dry_run {
                println!("(dry-run: signature.json not written)");
            } else {
                println!("wrote {}/signature.json", dir.display());
            }
            Ok(())
        }
        Cmd::Verify { dir, pubkey } => {
            let state = libsign::verify_plugin(&dir, pubkey.as_deref())?;
            println!("{}", state_label(&state));
            Ok(())
        }
        Cmd::Inspect { dir, pubkey } => {
            match libsign::read_signature_raw(&dir) {
                Ok(raw) => {
                    // 原样美化打印（已是 pretty JSON）。
                    println!("{raw}");
                }
                Err(e) => {
                    println!("(no signature.json: {e})");
                }
            }
            let state = libsign::verify_plugin(&dir, pubkey.as_deref())?;
            println!("verify: {}", state_label(&state));
            Ok(())
        }
        Cmd::CheckRegistry { registry, dir } => {
            let check = libsign::check_registry(&registry, dir.as_deref())?;
            for e in &check.errors {
                println!("error: {e}");
            }
            for w in &check.warnings {
                println!("warning: {w}");
            }
            println!(
                "checked {} 个版本：{} 错误 / {} 警告",
                check.versions,
                check.errors.len(),
                check.warnings.len()
            );
            if !check.errors.is_empty() {
                std::process::exit(1);
            }
            Ok(())
        }
    }
}

fn state_label(s: &SignState) -> &'static str {
    match s {
        SignState::Official => "official",
        SignState::ThirdParty => "third_party",
        SignState::Unsigned => "unsigned",
        SignState::Invalid => "invalid",
    }
}
