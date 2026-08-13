use std::path::PathBuf;

fn main() {
    // 嵌入与 UI 相同的 spark.ico：让 host 在所有场景（任务栏/Alt+Tab/搜索/资源管理器）
    // 都显示品牌图标，而不是 Windows 默认占位（快捷方式目标 exe 无图标时的表现）。
    //
    // 注意：不能直接把 .ico 传给 embed_resource::compile——它只编译 .rc 脚本，
    // 实测把 .ico 当脚本交给 rc.exe 时产出空的 spark.lib（32 字节），图标根本没进 exe
    // （v0.2.3 因此"内嵌图标"从未生效，搜索 Spark 一直显示占位）。必须显式生成
    // `1 ICON "…"` 脚本再编译。
    let ico = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap())
        .join("../../ui/Spark.UI/Assets/spark.ico");
    assert!(ico.exists(), "spark.ico not found: {}", ico.display());
    let rc = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("spark.rc");
    // rc 脚本里用正斜杠路径：rc.exe 两种都接受，正斜杠无需转义
    let script = format!("1 ICON \"{}\"\n", ico.display().to_string().replace('\\', "/"));
    std::fs::write(&rc, script).expect("write spark.rc");
    embed_resource::compile(&rc, embed_resource::NONE);
}
