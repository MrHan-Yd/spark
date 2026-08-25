//! 跨 crate 集成测试：sign-tool 签的 → plugin-manager 验过；sign-tool verify --pubkey 自检。
//!
//! 这些测试证明签名端（sign-tool，复用 spark-plugin-manager 的 canon）与验签端
//! （plugin-manager verify）对同一份 signature.json 字节一致、往返闭合（规范 §9.1 / 任务 2.2）。
//! 用每次生成的临时密钥，**不**依赖被 gitignore 的官方私钥（CI 无此文件）。

use spark_plugin_manager as pm;
use spark_sign as s;
use std::fs;
use std::path::Path;

fn make_plugin(dir: &Path, id: &str, version: &str) {
    fs::create_dir_all(dir).unwrap();
    let json = format!(
        r#"{{"id":"{id}","name":"T","version":"{version}","api_version":2,"runtime":"webview","main":"index.html","features":[{{"type":"keyword","keyword":"t","title":"T","mode":"page"}}]}}"#
    );
    fs::write(dir.join("plugin.json"), json).unwrap();
    fs::write(dir.join("index.html"), "<html></html>").unwrap();
    fs::create_dir_all(dir.join("assets")).unwrap();
    fs::write(dir.join("assets/style.css"), "body{}").unwrap();
}

/// 用一次临时密钥签的插件，经 plugin-manager `verify_with_keys` 验为 Official。
#[test]
fn sign_then_verify_with_keys_is_official() {
    let tmp = std::env::temp_dir().join("spark_sign_e2e_official");
    let _ = fs::remove_dir_all(&tmp);
    let plugin_dir = tmp.join("com.spark.x");
    make_plugin(&plugin_dir, "com.spark.x", "0.1.0");

    let sk = s::generate_keypair();
    let keyfile = tmp.join("k.key");
    s::write_private_key(&keyfile, &sk).unwrap();

    let out = s::sign_dir(&plugin_dir, &keyfile, "dev-v1", false).unwrap();
    assert_eq!(out.plugin_id, "com.spark.x");
    assert_eq!(out.version, "0.1.0");
    assert_eq!(out.key_id, "dev-v1");
    // 4 个文件：plugin.json + index.html + assets/style.css（signature.json 不列入）。
    assert_eq!(out.files.len(), 3);
    assert!(plugin_dir.join("signature.json").is_file());

    // 用同一公钥建可信表，plugin-manager 验签 → Official。
    let pubkey = s::pubkey_base64(&sk);
    let key = pm::TrustedKey {
        key_id: std::borrow::Cow::Owned("dev-v1".to_string()),
        algorithm: std::borrow::Cow::Borrowed("ed25519"),
        public_key: std::borrow::Cow::Owned(pubkey),
        kind: pm::KeyKind::Official,
        note: std::borrow::Cow::Borrowed("test"),
    };
    let state = pm::verify_with_keys(
        &plugin_dir,
        "com.spark.x",
        "0.1.0",
        std::slice::from_ref(&key),
        &[],
    )
    .unwrap();
    assert_eq!(state, pm::SignState::Official);
}

/// sign-tool 自身 verify --pubkey 路径往返闭合。
#[test]
fn sign_then_verify_pubkey_path() {
    let tmp = std::env::temp_dir().join("spark_sign_e2e_pubkey");
    let _ = fs::remove_dir_all(&tmp);
    let plugin_dir = tmp.join("com.spark.y");
    make_plugin(&plugin_dir, "com.spark.y", "0.2.0");

    let sk = s::generate_keypair();
    let keyfile = tmp.join("k.key");
    s::write_private_key(&keyfile, &sk).unwrap();
    s::sign_dir(&plugin_dir, &keyfile, "dev-v1", false).unwrap();

    let pubkey = s::pubkey_base64(&sk);
    let state = s::verify_plugin(&plugin_dir, Some(&pubkey)).unwrap();
    assert_eq!(state, pm::SignState::Official);

    // 不带 --pubkey（内置官方表）→ 这把开发密钥不在表里 → Invalid。
    let state_builtin = s::verify_plugin(&plugin_dir, None).unwrap();
    assert_eq!(state_builtin, pm::SignState::Invalid);
}

/// 签名后篡改一文件 → verify --pubkey 转 Invalid。
#[test]
fn tamper_after_sign_is_invalid() {
    let tmp = std::env::temp_dir().join("spark_sign_e2e_tamper");
    let _ = fs::remove_dir_all(&tmp);
    let plugin_dir = tmp.join("com.spark.z");
    make_plugin(&plugin_dir, "com.spark.z", "0.1.0");

    let sk = s::generate_keypair();
    let keyfile = tmp.join("k.key");
    s::write_private_key(&keyfile, &sk).unwrap();
    s::sign_dir(&plugin_dir, &keyfile, "dev-v1", false).unwrap();

    fs::write(plugin_dir.join("index.html"), "<html>hacked</html>").unwrap();
    let pubkey = s::pubkey_base64(&sk);
    let state = s::verify_plugin(&plugin_dir, Some(&pubkey)).unwrap();
    assert_eq!(state, pm::SignState::Invalid);
}

/// dry-run 不写 signature.json，只报告清单。
#[test]
fn dry_run_does_not_write() {
    let tmp = std::env::temp_dir().join("spark_sign_e2e_dryrun");
    let _ = fs::remove_dir_all(&tmp);
    let plugin_dir = tmp.join("com.spark.d");
    make_plugin(&plugin_dir, "com.spark.d", "0.1.0");

    let sk = s::generate_keypair();
    let keyfile = tmp.join("k.key");
    s::write_private_key(&keyfile, &sk).unwrap();
    let out = s::sign_dir(&plugin_dir, &keyfile, "dev-v1", true).unwrap();
    assert_eq!(out.files.len(), 3);
    assert!(!plugin_dir.join("signature.json").exists());
}

/// 内置官方公钥拒绝伪造签名（用非官方私钥签但 key_id 填官方）。
#[test]
fn builtin_official_rejects_forged_key_id() {
    let tmp = std::env::temp_dir().join("spark_sign_e2e_forged");
    let _ = fs::remove_dir_all(&tmp);
    let plugin_dir = tmp.join("com.spark.f");
    make_plugin(&plugin_dir, "com.spark.f", "0.1.0");

    let sk = s::generate_keypair();
    let keyfile = tmp.join("k.key");
    s::write_private_key(&keyfile, &sk).unwrap();
    // 故意 key_id 填官方：内置官方公钥验这把伪造签名 → Invalid。
    s::sign_dir(&plugin_dir, &keyfile, "spark-official-v1", false).unwrap();

    let state = s::verify_plugin(&plugin_dir, None).unwrap();
    assert_eq!(state, pm::SignState::Invalid);
}

/// 无 signature.json 时，`verify --pubkey` 与内置路径一致返回 Unsigned（不报错）。
#[test]
fn verify_pubkey_without_signature_is_unsigned() {
    let tmp = std::env::temp_dir().join("spark_sign_e2e_unsigned_pubkey");
    let _ = fs::remove_dir_all(&tmp);
    let plugin_dir = tmp.join("com.spark.u");
    make_plugin(&plugin_dir, "com.spark.u", "0.1.0");

    let sk = s::generate_keypair();
    let pubkey = s::pubkey_base64(&sk);
    let state = s::verify_plugin(&plugin_dir, Some(&pubkey)).unwrap();
    assert_eq!(state, pm::SignState::Unsigned);
    let state_builtin = s::verify_plugin(&plugin_dir, None).unwrap();
    assert_eq!(state_builtin, pm::SignState::Unsigned);
}

/// `sign` 写本地审计日志（规范 §9.2），含 key_id/plugin_id/version。
#[test]
fn sign_writes_audit_log() {
    let tmp = std::env::temp_dir().join("spark_sign_e2e_audit");
    let _ = fs::remove_dir_all(&tmp);
    let plugin_dir = tmp.join("com.spark.a");
    make_plugin(&plugin_dir, "com.spark.a", "0.3.0");

    let sk = s::generate_keypair();
    let keyfile = tmp.join("k.key");
    s::write_private_key(&keyfile, &sk).unwrap();
    s::sign_dir(&plugin_dir, &keyfile, "dev-v1", false).unwrap();

    let audit = keyfile.with_extension("audit.log");
    assert!(
        audit.is_file(),
        "audit log should exist at {}",
        audit.display()
    );
    let line = fs::read_to_string(&audit).unwrap();
    assert!(line.contains("dev-v1"));
    assert!(line.contains("com.spark.a"));
    assert!(line.contains("0.3.0"));
}

/// 本地专测：用仓库内 gitignored 的 `keys/spark-official-v1.key` 签，
/// 再用内置官方公钥 `verify_dir` 验为 Official。`#[ignore]`：CI 无该私钥，
/// 仅本机 `cargo test -- --ignored` 跑；缺文件时静默跳过。
#[test]
#[ignore]
fn official_key_roundtrip_local() {
    // 测试 CWD 是 crates/sign-tool，需回到仓库根定位 keys/。
    let keyfile = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("keys/spark-official-v1.key");
    if !keyfile.is_file() {
        eprintln!(
            "skipping: {} 不存在（CI 或未生成官方密钥）",
            keyfile.display()
        );
        return;
    }
    eprintln!("using official key: {}", keyfile.display());
    let tmp = std::env::temp_dir().join("spark_sign_e2e_builtin_official");
    let _ = fs::remove_dir_all(&tmp);
    let plugin_dir = tmp.join("com.spark.official");
    make_plugin(&plugin_dir, "com.spark.official", "0.1.0");

    s::sign_dir(&plugin_dir, &keyfile, "spark-official-v1", false).unwrap();
    let state = pm::verify_dir(&plugin_dir, "com.spark.official", "0.1.0").unwrap();
    assert_eq!(state, pm::SignState::Official);
}

// ─── check-registry：官方仓库准入（3.1 强制签名，规范 §12.2）──────────────

/// 构造"仓库根 + registry.json + 已签名版本目录"：sign 插件目录，从包内
/// signature.json 抽 key_id/signature 填进 registry 的 versions[].signature。
fn make_registry_repo(
    tmp: &Path,
    plugin_id: &str,
    version: &str,
    keyfile: &Path,
    key_id: &str,
) -> (std::path::PathBuf, std::path::PathBuf) {
    let repo = tmp.join("repo");
    let pkg_dir = repo.join("p").join(version);
    make_plugin(&pkg_dir, plugin_id, version);
    s::sign_dir(&pkg_dir, keyfile, key_id, false).unwrap();
    let pkg_sig: serde_json::Value =
        serde_json::from_str(&s::read_signature_raw(&pkg_dir).unwrap()).unwrap();
    let registry = serde_json::json!({
        "schema": 1,
        "name": "test repo",
        "plugins": [{
            "id": plugin_id,
            "name": "T",
            "runtime": "webview",
            "latest": version,
            "versions": [{
                "version": version,
                "path": format!("p/{version}"),
                "url": null,
                "sha256": null,
                "signature": {
                    "schema": 1,
                    "key_id": pkg_sig["key_id"],
                    "algorithm": "ed25519",
                    "signature": pkg_sig["signature"]
                }
            }]
        }]
    });
    let reg_path = tmp.join("registry.json");
    fs::write(&reg_path, serde_json::to_string_pretty(&registry).unwrap()).unwrap();
    (reg_path, repo)
}

fn test_key_table(sk: &ed25519_dalek::SigningKey) -> Vec<pm::TrustedKey> {
    vec![pm::TrustedKey {
        key_id: std::borrow::Cow::Owned("dev-v1".to_string()),
        algorithm: std::borrow::Cow::Borrowed("ed25519"),
        public_key: std::borrow::Cow::Owned(s::pubkey_base64(sk)),
        kind: pm::KeyKind::Official,
        note: std::borrow::Cow::Borrowed("test official dev key"),
    }]
}

/// 全量校验：registry 结构 + 包内验签一致 → 0 错误通过。
#[test]
fn check_registry_valid_repo_passes() {
    let tmp = std::env::temp_dir().join("spark_sign_checkreg_ok");
    let _ = fs::remove_dir_all(&tmp);
    let sk = s::generate_keypair();
    let keyfile = tmp.join("k.key");
    s::write_private_key(&keyfile, &sk).unwrap();
    let (reg_path, repo) = make_registry_repo(&tmp, "com.spark.x", "0.1.0", &keyfile, "dev-v1");

    let keys = test_key_table(&sk);
    let check = s::check_registry_with_keys(&reg_path, Some(&repo), &keys, &keys).unwrap();
    assert!(check.errors.is_empty(), "errors: {:?}", check.errors);
    assert_eq!(check.versions, 1);
    assert!(check.warnings.is_empty(), "warnings: {:?}", check.warnings);
}

/// 缺 signature 字段 → 准入错误（3.1 强制签名）。
#[test]
fn check_registry_missing_signature_rejects() {
    let tmp = std::env::temp_dir().join("spark_sign_checkreg_missing");
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).unwrap();
    let registry = serde_json::json!({
        "schema": 1,
        "name": "t",
        "plugins": [{
            "id": "com.spark.y",
            "name": "Y",
            "runtime": "webview",
            "latest": "0.1.0",
            "versions": [{ "version": "0.1.0", "path": "y/0.1.0", "url": null }]
        }]
    });
    let reg_path = tmp.join("registry.json");
    fs::write(&reg_path, serde_json::to_string(&registry).unwrap()).unwrap();

    let check = s::check_registry(&reg_path, None).unwrap();
    assert_eq!(check.errors.len(), 1);
    assert!(check.errors[0].contains("未签名"), "{}", check.errors[0]);
}

/// 伪造官方 key_id、坏 base64、错长度都拦下。
#[test]
fn check_registry_malformed_signature_fields_reject() {
    let tmp = std::env::temp_dir().join("spark_sign_checkreg_malformed");
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).unwrap();
    let cases = [
        // (key_id, signature_b64) → 期望的 error 子串
        ("forged-official-v9", "c2ln", "不是官方密钥"),
        ("spark-official-v1", "!!!not-base64!!!", "不是合法 base64"),
        ("spark-official-v1", "c2ln", "64 字节"),
    ];
    for (i, (key_id, sig, expect)) in cases.iter().enumerate() {
        let registry = serde_json::json!({
            "schema": 1,
            "name": "t",
            "plugins": [{
                "id": format!("com.spark.c{i}"),
                "name": "C",
                "runtime": "webview",
                "latest": "0.1.0",
                "versions": [{
                    "version": "0.1.0",
                    "path": "c/0.1.0",
                    "url": null,
                    "signature": { "schema": 1, "key_id": key_id, "algorithm": "ed25519", "signature": sig }
                }]
            }]
        });
        let reg_path = tmp.join(format!("r{i}.json"));
        fs::write(&reg_path, serde_json::to_string(&registry).unwrap()).unwrap();
        let check = s::check_registry(&reg_path, None).unwrap();
        assert_eq!(check.errors.len(), 1, "case {i}");
        assert!(
            check.errors[0].contains(expect),
            "case {i}: {}",
            check.errors[0]
        );
    }
}

/// --dir 给仓库根：包内缺 signature.json、包内被篡改会被准入拒绝
/// （包内是权威，§4.6；registry 的 signature 字段替代不了包内验签）。
#[test]
fn check_registry_package_verify_is_authoritative() {
    let tmp = std::env::temp_dir().join("spark_sign_checkreg_pkg");
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).unwrap();
    let sk = s::generate_keypair();
    let keyfile = tmp.join("k.key");
    s::write_private_key(&keyfile, &sk).unwrap();
    let (reg_path, repo) = make_registry_repo(&tmp, "com.spark.p", "0.1.0", &keyfile, "dev-v1");
    let keys = test_key_table(&sk);

    // 篡改包内文件后：registry 字段仍是旧签名 → 包内全量重验 Invalid → 拒绝。
    fs::write(
        repo.join("p").join("0.1.0").join("index.html"),
        "<html>hacked</html>",
    )
    .unwrap();
    let check = s::check_registry_with_keys(&reg_path, Some(&repo), &keys, &keys).unwrap();
    assert_eq!(check.errors.len(), 1, "errors: {:?}", check.errors);
    assert!(
        check.errors.iter().any(|e| e.contains("包内验签失败")),
        "errors: {:?}",
        check.errors
    );

    // 删掉包内 signature.json：包内陷 Unsigned → 拒绝（registry 字段替代不了包内权威）。
    fs::remove_file(repo.join("p").join("0.1.0").join("signature.json")).unwrap();
    let check = s::check_registry_with_keys(&reg_path, Some(&repo), &keys, &keys).unwrap();
    assert_eq!(check.errors.len(), 1, "errors: {:?}", check.errors);
    assert!(
        check
            .errors
            .iter()
            .any(|e| e.contains("包内缺 signature.json")),
        "errors: {:?}",
        check.errors
    );
}

/// registry schema 非 1 → 整体报错。
#[test]
fn check_registry_unsupported_schema_fails() {
    let tmp = std::env::temp_dir().join("spark_sign_checkreg_schema");
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).unwrap();
    let registry = serde_json::json!({ "schema": 2, "name": "t", "plugins": [] });
    let reg_path = tmp.join("registry.json");
    fs::write(&reg_path, serde_json::to_string(&registry).unwrap()).unwrap();
    let err = s::check_registry(&reg_path, None).unwrap_err();
    assert!(err.to_string().contains("schema"));
}
