# Spark 插件签名密钥目录

本目录存放 **Spark 官方插件签名密钥**的私钥文件（`*.key`），已被 `.gitignore` 排除，**绝不提交仓库**。

## 文件

- `spark-official-v1.key` — Ed25519 私钥（base64 seed），gitignored。用 `spark-sign` 签官方插件时通过 `--key` 引用。

## 当前状态（开发密钥）

`crates/plugin-manager/src/signing/keys.rs` 内置的 `spark-official-v1` 公钥是**开发密钥**——
本机用 `spark-sign keygen` 生成，私钥就在本目录（仅本机存在）。它用于让签名链路在开发期端到端可跑。

## 正式发布前必须重新生成（离线机）

按规范 §5.2，正式官方密钥**绝不能**在联网的开发机生成。发布前：

1. 在一台**离线机**上运行：
   ```sh
   cargo run -p spark-sign --release -- keygen --out spark-official-v1.key --key-id spark-official-v1
   ```
2. 把打印的 base64 公钥填回 `crates/plugin-manager/src/signing/keys.rs` 的 `TRUSTED_KEYS` 条目（替换开发公钥）。
3. 私钥文件拷贝到 **GitHub Actions 加密 secret**（如 `SPARK_SIGNING_KEY_V1`），仅 release 构建时注入；离线原件按规范保管（加密介质 / HSM / YubiKey）。
4. 删除本机开发私钥，确认 `git status` 不出现任何 `*.key`。

## 验证

```sh
# 用内置官方公钥验签（确认 keys.rs 公钥与此处私钥配套）
cargo run -p spark-sign -- verify --dir <已签名插件目录>
```