# 插件签名三期 — 实现任务清单

> 规范文档：[`插件开发/插件签名规范.md`](../插件开发/插件签名规范.md)
> 创建时间：2026-08-25
> 状态：Phase 0（文档）已完成，Phase 1-4 待实施

---

## 总览

插件签名分 4 个 Phase，可跨多次会话完成。

**阶段依赖**：Phase 0（文档）✅ → Phase 1（Rust 核心：canonicalization + 验签 + 内置公钥，纯逻辑可独立测）→ Phase 2（sign-tool CLI + 官方密钥 + CI 签名）→ Phase 3（install/scan 接入 + PluginInfo + 错误处理）→ Phase 4（UI 角标 + 安装提示 + 市场字段，依赖 Phase 3 的 `sign_state`）。

**架构要点**：验签在 host（`plugin-manager`）完成；canonicalization 函数被 `plugin-manager` 与 `sign-tool` 共用；不新增 IPC 方法（签名是 `host.plugin.install`/`scan_standard` 内部步骤）；`PluginInfo.sign_state` 随 `host.plugin.list` 自然返回。**唯一新增 Rust 依赖**：`ed25519-dalek`、`sha2`、`base64`（+ `rand`/`rand_core` 仅 sign-tool）。详见规范 §13。

---

## Phase 0：规范文档 ✅ 已完成

- [x] 新建 `插件开发/插件签名规范.md` — 完整规范
- [x] 新建本任务清单

---

## Phase 1：Rust 核心（canonicalization + 验签 + 内置公钥）

> 纯逻辑，不依赖 install/scan 流程，可独立 `cargo test`。

### 1.1 新增依赖
- [x] `crates/plugin-manager/Cargo.toml` 加 `ed25519-dalek = "2"`、`sha2 = "0.10"`、`base64 = "0.22"`
- [x] workspace `Cargo.lock` 更新（联网拉取一次，后续可 vendor）

### 1.2 signing 模块
- [x] `crates/plugin-manager/src/signing/mod.rs`
  - `pub struct TrustedKey { key_id, algorithm, public_key, kind }`
  - `pub enum KeyKind { Official, ThirdParty }`
  - `pub const TRUSTED_KEYS: &[TrustedKey]`（含 `spark-official-v1` 公钥占位，待真实密钥生成后替换）
  - `pub const REVOKED: &[Revocation]`（v1 空表）
- [x] `crates/plugin-manager/src/signing/canon.rs`
  - `pub fn canonical_bytes(plugin_id, version, algorithm, key_id, files: &[FileEntry]) -> Vec<u8>`
  - 按 §4.4 规则拼字节，files 按 path 字典序排序
  - 单测：固定输入 → 固定字节（golden test）
- [x] `crates/plugin-manager/src/signing/verify.rs`
  - `pub enum SignState { Official, ThirdParty, Unsigned, Invalid }`
  - `pub fn verify_dir(dir: &Path, expected_id: &str, expected_version: &str) -> Result<SignState, VerifyError>`
  - 读 `signature.json` → schema/plugin_id/version/algorithm 校验 → 重算每个 file 的 SHA-256 → 重建 canonical bytes → Ed25519 verify against `TRUSTED_KEYS` → 命中 `REVOKED` 视为 Invalid
  - 单测：官方签名样例验过、篡改一文件后 Invalid、无 signature.json 返回 Unsigned、id/version 不匹配报错

### 1.3 接入 lib
- [x] `crates/plugin-manager/src/lib.rs` 加 `mod signing;` 与 `pub use signing::{SignState, ...}`

### 1.4 质量门禁
- [x] `cargo fmt`
- [x] `cargo test --workspace`
- [x] Code Auditor 审计（子智能体不稳定，主线程自审 PASSED）

### 涉及文件
```
crates/plugin-manager/Cargo.toml
crates/plugin-manager/src/lib.rs
crates/plugin-manager/src/signing/mod.rs      (新)
crates/plugin-manager/src/signing/canon.rs    (新)
crates/plugin-manager/src/signing/verify.rs   (新)
```

---

## Phase 2：签名工具 + 官方密钥 + CI 签名

### 2.1 sign-tool crate
- [x] workspace `Cargo.toml` `members` 加 `crates/sign-tool`
- [x] `crates/sign-tool/Cargo.toml`：依赖 `spark-plugin-manager`（共用 signing 模块的 canon）+ `ed25519-dalek` + `base64` + `rand` + `clap` + `anyhow`（sha2 经 plugin-manager 间接复用，无需直接依赖）
- [x] `crates/sign-tool/src/main.rs` + `src/lib.rs`（核心逻辑抽到 lib 供集成测试）：
  - `keygen --out <keyfile> --key-id <id>`：生成 Ed25519 密钥对，写 base64 私钥文件，打印 base64 公钥
  - `sign --dir <plugin_dir> --key <keyfile> --key-id <id>`：算文件哈希、拼 canonical bytes、签名、写 `<dir>/signature.json`（dry-run 打印文件清单）
  - `verify --dir <plugin_dir> [--pubkey <b64>]`：调 `verify_dir`（内置公钥表）或 `verify_with_keys`（指定公钥自检）
  - `inspect --dir <plugin_dir> [--pubkey <b64>]`：打印 signature.json 与验签结果

### 2.2 canonicalization 共享
- [x] 确认 `sign-tool` 与 `plugin-manager` 共用 `spark_plugin_manager::signing::canon::canonical_bytes` + `collect_file_entries`（同一份代码，无两端漂移）
- [x] 集成测试：sign-tool 签的 → plugin-manager `verify_with_keys` 验过（Official）；sign-tool `verify --pubkey` 往返；篡改→Invalid；伪造 key_id→Invalid；本机官方密钥 round-trip（ignored）
- [x] **Web 签名页** `tools/plugin-signer.html`：单文件浏览器工具，给非 CLI 场景的官方签名者用。
  - canonical bytes 逐字节移植自 `canon.rs`（node 对拍 golden test 通过）；Ed25519 用 `@noble/ed25519`（与 `ed25519-dalek` 同为 RFC 8032）。
  - 两种取文件方式：GitHub 仓库（填 owner/repo+分支+版本目录路径，经 GitHub API 抓文件，可填 PAT 直接 commit 回仓库）/ 本地目录（文件夹选择器）。
  - 产出 `signature.json` 与 `spark-sign` CLI 字节级一致；**端到端对拍**：node(@noble) 用开发密钥签真实插件目录 → Rust `spark-sign verify` 判 `official`。
  - 私钥仅在浏览器内存（可选 localStorage 记忆）；自验（派生公钥验自己签名）。私钥保管见 §2.3。

### 2.3 官方密钥生成与内置
- [ ] ⚠️ 离线机跑 `spark-sign keygen` 生成**正式** `spark-official-v1` 密钥对（当前内置的是开发密钥，发布前须替换，见 keys/README.md）
- [x] 开发公钥填入 `crates/plugin-manager/src/signing/keys.rs` 的 `TRUSTED_KEYS`（带"发布前需离线机重生成"告警注释）
- [x] 私钥按 §5.2 保管：开发私钥在 `keys/spark-official-v1.key`（gitignored）；正式私钥走 CI secret
- [x] 仓库加 `.gitignore` 确保私钥路径不被提交（`keys/*.key` + `keys/README.md` 白名单）

### 2.4 官方插件 CI 签名
- [x] Release workflow 加签名步骤：构建 spark-sign + 条件签名 `plugins/dist/*/`（缺 secret 或无打包目录即 no-op，不阻塞 release）
- [ ] 插件打包流水线（native 插件 build exe → 装配 dist 目录）就绪后该步骤自动生效（属市场二期打包范畴）
- [ ] 签名日志落 CI 产物（不入仓库）— 待打包流程落地后补

### 2.5 质量门禁
- [x] `cargo fmt`
- [x] `cargo test --workspace`（plugin-manager 53 + sign-tool 9 + 集成 5 全绿）
- [x] Code Auditor 审计（子智能体不稳定，主线程自审 PASSED）

### 涉及文件
```
Cargo.toml                              (members 加 sign-tool)
crates/sign-tool/Cargo.toml             (新)
crates/sign-tool/src/lib.rs             (新)
crates/sign-tool/src/main.rs            (新)
crates/sign-tool/tests/integration.rs   (新)
crates/plugin-manager/src/signing/keys.rs (拆分 + 开发公钥)
crates/plugin-manager/src/signing/canon.rs (collect_file_entries 公开)
.github/workflows/release.yml           (签名步骤)
.gitignore                              (排除私钥路径)
keys/README.md                          (新，密钥保管说明)
```

---

## Phase 3：install/scan 接入 + PluginInfo + 错误处理 ✅ 已完成

### 3.1 install_from_dir 接入验签
- [x] `crates/plugin-manager/src/lib.rs` `install_from_dir` 加参数 `require_signature: bool`
  - 读清单后、拷贝前调 `verify::verify_dir`（对源目录全量验签）
  - `require_signature=true` 且无 `signature.json` → `Err(SignatureMissing)`
  - 官方密钥验失败 → `Err(SignatureInvalid)`
  - 无签名且 `require_signature=false` → 记 `SignState::Unsigned`，继续
  - 验过 → 记 `SignState::Official`/`ThirdParty`
  - `PluginInstallOutcome` 加 `sign_state` 字段
  - `verify_dir` 返回 `Ok(SignState)`，`Invalid` 不抛 Err；install 侧据策略：`Ok(Invalid)` 一律转 `Err(SignatureInvalid)`，`Unsigned`+`require_signature` 转 `Err(SignatureMissing)`
  - 同步更新既有调用 `install_from_dir` 的单测（补 `require_signature` 实参）
- [x] `crates/plugin-manager/src/error.rs` 加 `SignatureInvalid(String)` / `SignatureMissing(String)` 变体 + `Verify(#[from] VerifyError)`

### 3.2 IPC 调用方传参
- [x] `crates/ipc/src/protocol.rs` `PluginInstallParams` 加 `#[serde(default)] require_signature: bool`（默认 false，老 UI 不传仍兼容）
- [x] `crates/host/src/app.rs` 的 `plugin_install` wrapper 增透传 `require_signature` 到 `install_from_dir`
- [x] `crates/host/src/ipc_server.rs` `host.plugin.install` handler 透传参数到 `plugin_install`

### 3.3 LoadedPlugin / PluginInfo
- [x] `LoadedPlugin` 加 `sign_state: SignState`
- [x] `PluginInfo` 加 `pub sign_state: SignState`
- [x] `list()` 填充 sign_state（install 时已记，直接读；scan_standard 时重验，见 3.4）
- [x] serde：`SignState` `#[serde(rename_all = "snake_case")]`，UI 收到 `official`/`third_party`/`unsigned`/`invalid`

### 3.4 scan_standard 重验（v1 轻量版）
- [x] `scan_dir_with_source` 扫到 `signature.json` 存在时，`verify_dir_light`：校验 plugin.json 哈希 + signature.json schema + Ed25519 验签（不全量重算资源文件，留 Phase 5 全量）
- [x] 验签失败记 `Invalid`，UI 禁用提示
- [x] 无 `signature.json` 记 `Unsigned`，存量插件不拦截
- [x] dev 插件恒 `Unsigned`（不参与签名体系）

### 3.5 质量门禁
- [x] `cargo fmt`
- [x] `cargo test --workspace`（plugin-manager 61 + 全 workspace 绿）
- [x] Code Auditor 审计 PASSED（无阻断级缺陷；已按非阻断建议补 plugin.json 轻量校验、dev 恒 Unsigned、注释修正）

### 涉及文件
```
crates/plugin-manager/src/lib.rs
crates/plugin-manager/src/error.rs
crates/plugin-manager/src/signing/canon.rs    (公开 sha256_hex)
crates/plugin-manager/src/signing/verify.rs  (verify_dir_light + full_rehash 分支)
crates/plugin-manager/src/signing/mod.rs     (re-export verify_dir_light)
crates/ipc/src/protocol.rs
crates/host/src/app.rs                        (plugin_install wrapper 透传)
crates/host/src/ipc_server.rs
```

---

## Phase 4：UI 角标 + 安装提示 + 市场 signature 字段

> 状态：4.1/4.2 已完成（Code Auditor 回归通过）；4.3/4.4 待市场页落地后再做；4.5 文档同步待做。

### 4.1 C# DTO ✅
- [x] `Models/PluginDto.cs` 加 `sign_state` 字段（string，承接 host snake_case）+ `PluginSignState` 枚举（VM 解析用）
- [ ] ~~`Models/RegistryDto.cs` `RegistryVersionDto` 加 `Signature` 可选字段~~ — 市场页（`RegistryDto`/`RegistryService`）尚未落地，随市场二期一起做

### 4.2 设置·插件列表 ✅
- [x] 插件项加 `SignState` 角标：官方=绿色"官方"、三方=蓝色"已签名"、未签名=无、失效=红色"签名失效"
- [x] `Invalid` 时不阻止"关闭"，仅阻止"启用"（`OnPluginToggled` 拦截 Off→On，保留 On→Off 处置路径）+ 红色提示行（如实告知"文件可能被篡改，建议停用或卸载"，不虚构"已禁用"）
- [x] Code Auditor 回归 PASSED（Issue #1 开关锁死、Issue #2 文案误报 两个阻断级已修复）

### 4.3 市场·插件卡片（待市场页落地）
- [ ] 卡片标题旁徽章（官方/已签名/未签名）— `MarketplacePage` 尚未创建
- [ ] 未签名安装前弹"未签名，确认来源可信"确认框
- [ ] 官方仓库未签名版本（3.0 过渡期）显示"待签名"灰徽，仍可装但警告

### 4.4 registry.json signature 字段处理（待市场页落地）
- [ ] `RegistryService` 下载后若 `version.signature` 有值，与包内 `signature.json` 比对（不一致以包内为准，仅记日志）— `RegistryService` 尚未创建
- [ ] 卡片优先用 `version.signature.key_id` 预判"官方"（下载前展示），安装时以 host 验签结果为准

### 4.5 文档同步 ✅
- [x] `插件开发/插件开发规范.md` §9.5 发布（CI 自动签名 + 三方待后续）/ §12 安全约束（官方私钥签名 + 本地免签名）/ §14 清单 schema（反注释不加 official/signed 字段）/ §15 IPC 表（install 内部验签 + list 返 sign_state）
- [x] `插件开发/插件市场与仓库.md` §3.3 versions 加 `signature` 可选字段 / §5 安装流程加 signature.json 验签步骤 / §13.5 安全能力表 代码签名 ❌→✅ / §13.6 事件响应 仓库被入侵兜底
- [x] `插件开发/WebView插件开发.md` §9.2 + `插件开发/Native插件开发.md` §8 打包发布章节加"官方插件随 CI 自动签名"（native 不叠加 Authenticode）

### 4.6 质量门禁
- [x] UI 编译：`dotnet build` 0 错误（仅预存 CS1998 警告）
- [x] Rust 基线：`cargo fmt --check` + `cargo test --workspace` 全绿（plugin-manager 61；本阶段无 Rust 改动）
- [x] Code Auditor 审计 PASSED（2 个阻断级缺陷已修复并回归通过）
- [ ] UI 联调（需真机运行 host+UI）：官方签名插件装后显"官方"角标；篡改一文件后 scan 显"签名失效"且开关只能关不能开 — 待人工联调

### 涉及文件（已改）
```
ui/Spark.UI/Models/PluginDto.cs            (sign_state 字段 + PluginSignState 枚举)
ui/Spark.UI/Models/PluginInstallOutcomeDto.cs (sign_state 字段)
ui/Spark.UI/ViewModels/PluginRowVm.cs     (角标文案/画刷/可见性 + ParseSignState)
ui/Spark.UI/MainWindow.xaml               (行模板加签名角标 + 失效提示行)
ui/Spark.UI/MainWindow.xaml.cs            (OnPluginToggled 拦截 Invalid 插件启用)
```

### 涉及文件（待市场页落地）
```
Models/RegistryDto.cs         (未创建)
Services/RegistryService.cs    (未创建)
Views/MarketplacePage.xaml(.cs) (未创建)
插件开发/*.md
```

---

## Phase 5：3.1+ 增强 ✅ 已完成

- [x] 官方仓库新版本强制签名（仓库准入规则文档 + `spark-sign check-registry` 校验脚本）
  - `crates/sign-tool/src/lib.rs` `check_registry` / `check_registry_with_keys`：registry.json 结构校验 + signature 字段白名单 + 包内 verify_dir 必为 Official
  - CLI 子命令 `spark-sign check-registry --registry <path> [--dir <repo_root>]`
  - 集成测试 5 例（有效通过 / 缺签名拒绝 / 字段格式校验 / 包内验签权威 / schema 校验）
- [x] 启动期全量重验（scan_standard 重算所有文件哈希，3.2）
  - `sign_state_scanned` 调用 `verify_with_keys`（全量重算），`scan_dir_with_source` 已接入
  - 前序会话删除了 `verify_dir_light` 轻量分支，`verify_with_keys` 成为唯一主函数
- [x] 本地吊销列表生效（`is_revoked` 已在 `verify_with_keys` 验签路径调用；`REVOKED` 空表是设计默认——吊销是应急动作，发现恶意/泄露后随 host 紧急版本填入条目）
- [x] host"严格模式"开关（`HostConfig.strict_mode` + `SetConfigParams.strict_mode` + IPC 透传 + `effective_require = require_signature || config.strict_mode` + 设置页 ToggleButton 开关）
- [x] 三方签名：`HostConfig.trusted_pubkeys` + `set_trusted_user_keys` 运行时合并表 + 设置"受信任开发者"管理 UI（添加/删除公钥 + ItemsControl 列表）+ `ThirdParty` 角标（蓝色"已签名"）

---

## 跨 Phase 验收清单

- [ ] 官方签名插件：装后 `host.plugin.list` 返回 `sign_state=official`，UI 绿角标 — 待人工联调
- [ ] 篡改官方插件单文件：install 拒装 / scan 显 `invalid` 禁用 — 待人工联调
- [ ] 无签名插件（本地导入）：装后 `sign_state=unsigned`，UI 无角标，不拦截 — 待人工联调
- [ ] 私钥文件不在仓库（`.gitignore` 生效 + CI secret 注入）
- [x] `cargo fmt` + `cargo test --workspace` 全绿
- [x] Code Auditor PASSED（Issue #1 回滚重入已修复并回归通过）