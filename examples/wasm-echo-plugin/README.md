# WASM Component Extension

下列命令从仓库或交付包根目录执行。`local` 和 `all` 交付包包含 `ternilo-plugin`，请先将包内 `bin/` 加入 `PATH`；`server` 和 `worker` 包需另行安装该 CLI。从源码开发时，可将 `ternilo-plugin` 替换为 `cargo run --locked -p ternilo-plugin-cli --`。

该示例是统一 Extension Package 的 `wasm-component` 后端，只导入 `ternilo:extension/host@1.0.0`，不链接 WASI。一个 package 通过签名 manifest 注册有序的 `wasm-extension-guidance` Prompt section 和静态 `wasm-extension-tools` Skill，并由 Component 通过通用 WIT `invoke(handler, context-json, input-json)` 分发 Tool 或 Hook handler；本示例实现 `wasm_echo` 和 `wasm_read_workspace` 两个 Tool。Prompt 与 Skill 是声明式内容，不调用 WASM handler。构建、componentize、签名和验证：

```bash
rustup target add wasm32-unknown-unknown
cargo build --manifest-path examples/wasm-echo-plugin/Cargo.toml \
  --target wasm32-unknown-unknown --release

ternilo-plugin componentize \
  --module examples/wasm-echo-plugin/target/wasm32-unknown-unknown/release/ternilo_extension_wasm_echo.wasm \
  --output /tmp/ternilo-echo.component.wasm

ternilo-plugin keygen \
  --key-id example-publisher --output /tmp/ternilo-example-key.json
ternilo-plugin publisher \
  --key /tmp/ternilo-example-key.json \
  --source https://plugins.ternilo.dev/examples \
  --output /tmp/ternilo-example-publisher.json
ternilo-plugin sign \
  --manifest examples/wasm-echo-plugin/manifest.json \
  --payload /tmp/ternilo-echo.component.wasm \
  --key /tmp/ternilo-example-key.json \
  --output /tmp/ternilo-echo.bundle.json
ternilo-plugin verify \
  --bundle /tmp/ternilo-echo.bundle.json \
  --publisher /tmp/ternilo-example-publisher.json
```

`manifest.json` 中的 `payload_sha256` 是签名前占位值。`sign` 会读取 componentized WASM payload、计算真实摘要并写入签名 bundle；`verify` 再校验 publisher、source、签名和摘要，并通过临时 Extension Registry 执行与安装时相同的 WASM Component 编译验证。

安装时 operator 还要从 manifest 请求的 capability 中选择实际 grant。本示例的 `wasm_read_workspace` 先调用宿主日志再读取文件，因此需要同时授予 `log` 和 `workspace_read`；`wasm_echo` 不调用这两个宿主能力。Profile 与 Rhai 使用相同的 Extension mount：

```json
{
  "id": "extension:dev.ternilo.echo@1.0.0",
  "kind": "ternilo.extension.package",
  "enabled": true,
  "config": {
    "package_id": "dev.ternilo.echo",
    "version": "1.0.0",
    "settings": {}
  }
}
```
