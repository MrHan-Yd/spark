# 插件能力 e2e（管道直连，免 UI）

三套脚本直接连 `\\.\pipe\spark.host.ipc` 发 NDJSON 验证 host 侧行为，不依赖 UI。
运行前 `taskkill /IM spark-host.exe /F`（并杀 Spark.exe——安装版 watchdog 会拉
安装版 host 抢单实例）；脚本各自在 %TEMP% 起隔离的便携 data 目录 + `--plugins-dir`。
全部 ASCII only；所有 JSON 写盘/管道写用 `UTF8Encoding($false)`（无 BOM）。

| 脚本 | 覆盖 |
|------|------|
| `sign-state.ps1` | 签名验收：官方签 → `sign_state=official`；篡改副本 → 拒装；无签名 → `unsigned`；装后篡改 + 重启 → 启动期重验 `invalid`。依赖 `target/release/spark-sign.exe` + `keys/spark-official-v1.key`（开发密钥）。 |
| `plugin-caps.ps1` | `type: regex` / `type: root` 触发（候选 + `plugin_input`/`plugin_command`）、fs 范围内外读写（`fs_scopes` grant）、shell 控制字符拒绝、`host.plugin.list` 暴露 `fs_scopes`。 |
| `clipboard-image.ps1` | `clipboard.readImage` 真实剪贴板数据全链路：手工 DIB（40 头/BI_RGB/32bpp/2x2，字节确定）→ host GetClipboardData → RGBA → WIC PNG → base64 → System.Drawing GetPixel 断言颜色集合；空剪贴板 → `data:null`。需 `-STA`。 |
| `clipboard-item.ps1` | `clipboard.write`/`read`（多格式）：`write_item{text+image}` → `read_all` 两种格式都在且 2x2 PNG 四像素经 PNG→WIC 解码→CF_DIB→读回全链路不变色；text-only 写入后 image 格式消失（证明 `EmptyClipboard` 生效）；`INVALID_ARGS`/`UNSUPPORTED_FORMAT` 拒绝；**被拒写入不破坏剪贴板原有内容**；旧 `read_text`/`write_text` 仍通；操作后 host 仍响应。**会覆盖系统剪贴板内容**，且需要能打开剪贴板的交互桌面会话（无头/服务会话会在任何 Spark 代码之前就报 `CLIPBRD_E_CANT_OPEN`，同 `clipboard-image.ps1`）。 |
| `market-local.ps1` | 本地插件仓库 fixture + 抓取联通性：按 `plugins\hello`/`plugins\echo` 现打包 zip 并按真实字节算 sha256 写进 `registry.json`（tags / 无 tags / 超限 tags / 缺 latest 四种条目 + 三个畸形索引），起一次性 HTTP 服务后断言索引可达且解析、每个 `url` 版本可下载且 sha256 与索引一致、zip 根含 `plugin.json`、畸形索引仍畸形。**不覆盖** C# 侧 `RegistryService` 的解析容错与安装链路（UI 进程，本仓库无 C# 测试工程）——脚本末尾打印手工 UI 联调步骤。`-PythonExe <path>` 可在 PATH 受限的 shell 里指定 python。 |

用法（仓库根）：

```powershell
powershell -STA -NoProfile -ExecutionPolicy Bypass -File scripts/e2e/sign-state.ps1
powershell -STA -NoProfile -ExecutionPolicy Bypass -File scripts/e2e/plugin-caps.ps1
powershell -STA -NoProfile -ExecutionPolicy Bypass -File scripts/e2e/clipboard-image.ps1
powershell -STA -NoProfile -ExecutionPolicy Bypass -File scripts/e2e/clipboard-item.ps1
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/e2e/market-local.ps1
```

注意：e2e 前先 `cargo build -p spark-host`（`cargo test` 不重链 bin 产物，跑的是旧 exe）。