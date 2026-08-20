//! 插件私有键值存储（`<data_dir>/plugin-data/<id>/db.json`）。
//!
//! 一期实现：整文件 JSON map，读写均加载/保存全量。量小够用；
//! 后续量大可换 SQLite per-plugin。

use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

type Kv = BTreeMap<String, Value>;

fn db_path(data_dir: &Path, plugin_id: &str) -> PathBuf {
    data_dir.join("plugin-data").join(plugin_id).join("db.json")
}

fn load(data_dir: &Path, plugin_id: &str) -> Kv {
    match fs::read_to_string(db_path(data_dir, plugin_id)) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
        Err(_) => Kv::new(),
    }
}

fn save(data_dir: &Path, plugin_id: &str, kv: &Kv) -> io::Result<()> {
    let path = db_path(data_dir, plugin_id);
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let text =
        serde_json::to_vec_pretty(kv).map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
    fs::write(&path, &text)
}

/// 执行一条 db 方法调用，返回结果数据（供 `host.plugin.api` capability=db 直接回传）。
pub fn invoke(data_dir: &Path, plugin_id: &str, method: &str, args: Value) -> io::Result<Value> {
    let mut kv = load(data_dir, plugin_id);
    match method {
        "set" => {
            #[derive(serde::Deserialize)]
            struct SetArgs {
                key: String,
                value: Value,
            }
            let a: SetArgs = serde_json::from_value(args)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
            kv.insert(a.key, a.value);
            save(data_dir, plugin_id, &kv)?;
            Ok(Value::Bool(true))
        }
        "get" => {
            #[derive(serde::Deserialize)]
            struct GetArgs {
                key: String,
            }
            let a: GetArgs = serde_json::from_value(args)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
            Ok(kv.get(&a.key).cloned().unwrap_or(Value::Null))
        }
        "remove" => {
            #[derive(serde::Deserialize)]
            struct RemoveArgs {
                key: String,
            }
            let a: RemoveArgs = serde_json::from_value(args)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
            kv.remove(&a.key);
            save(data_dir, plugin_id, &kv)?;
            Ok(Value::Bool(true))
        }
        "keys" => Ok(Value::Array(
            kv.keys().cloned().map(Value::String).collect(),
        )),
        "clear" => {
            kv.clear();
            save(data_dir, plugin_id, &kv)?;
            Ok(Value::Bool(true))
        }
        other => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("unknown db method: {other}"),
        )),
    }
}
