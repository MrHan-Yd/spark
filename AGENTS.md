# Spark — Agent / 协作者备忘

- 技术栈定稿：`docs/TECH_STACK.md`（Rust host + C# WinUI 3 UI）
- 热路径逻辑只进 `crates/`，不要把索引/热键做进 UI
- HTML `ui-prototype/` 仅设计参考，不是生产 UI
- 提交前：`cargo test --workspace`；有 Rust 改动时 `cargo fmt`
