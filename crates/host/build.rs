fn main() {
    // 嵌入与 UI 相同的 spark.ico：让 host 在所有场景（任务栏/Alt+Tab/搜索/资源管理器）
    // 都显示品牌图标，而不是 Windows 默认占位（快捷方式目标 exe 无图标时的表现）。
    // 路径相对 CARGO_MANIFEST_DIR（crates/host/）。失败时 embed-resource 自行 panic。
    embed_resource::compile("../../ui/Spark.UI/Assets/spark.ico", embed_resource::NONE);
}
