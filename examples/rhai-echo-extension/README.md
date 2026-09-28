# Rhai Echo Extension

下列命令从仓库或交付包根目录执行。`local` 和 `all` 交付包包含 `ternilo-plugin`，请先将包内 `bin/` 加入 `PATH`；`server` 和 `worker` 包需另行安装该 CLI。从源码开发时，可将 `ternilo-plugin` 替换为 `cargo run --locked -p ternilo-plugin-cli --`。

这是一个可直接审查源码的 Rhai Extension Package 示例。一个 package 在同一份签名 manifest 中贡献一个有序 Prompt section、一个静态 Skill 和两个工具：

- `rhai_echo` 使用挂载设置中的 `prefix` 处理 `arguments.text`；
- `rhai_inspect` 返回传入参数、挂载设置中的 `label` 和宿主提供的安全会话标识。

`rhai-extension-guidance` 会随同一 Profile mount 注册到系统提示词；`rhai-extension-tools` 会进入现有 Skill 目录并可由模型或用户显式加载。停用、撤销、卸载或移除挂载时，Prompt、Skill provider 与两个工具按同一生命周期一起移除。Prompt 和 Skill 都是签名声明式内容，不执行 Rhai handler；只有 Tool contribution 进入 Rhai runtime。

该版本声明 `requested_capabilities: []`。脚本没有文件、网络、进程、环境变量、动态 `import` 或 `eval` 能力；它只能处理宿主传入的 `context`、`arguments` 和 `settings`。

## 生成密钥并签名

以下命令均拒绝覆盖已有输出文件。重新执行前请换一组输出路径或自行清理本次生成的临时文件。

```bash
ternilo-plugin keygen \
  --key-id example-publisher \
  --output /tmp/ternilo-rhai-example-key.json

ternilo-plugin publisher \
  --key /tmp/ternilo-rhai-example-key.json \
  --source https://plugins.ternilo.dev/examples \
  --output /tmp/ternilo-rhai-example-publisher.json

ternilo-plugin sign \
  --manifest examples/rhai-echo-extension/manifest.json \
  --payload examples/rhai-echo-extension/extension.rhai \
  --key /tmp/ternilo-rhai-example-key.json \
  --output /tmp/ternilo-rhai-example.bundle.json

ternilo-plugin verify \
  --bundle /tmp/ternilo-rhai-example.bundle.json \
  --publisher /tmp/ternilo-rhai-example-publisher.json
```

`manifest.json` 中的 `payload_sha256` 是签名前占位值。`sign` 根据 `runtime.kind` 把 `.rhai` 文件保存为 UTF-8 payload，计算真实摘要并写入签名 bundle。`verify` 会校验 publisher、source、签名和摘要，再通过临时 Extension Registry 执行与安装时相同的 Rhai 编译验证。

## 挂载设置

安装完成后，Profile 中的通用 Extension row 只引用 package/version，并把 manifest `config_schema` 对应的值放入 `settings`：

```json
{
  "id": "extension:dev.ternilo.rhai-echo@1.0.0",
  "kind": "ternilo.extension.package",
  "enabled": true,
  "config": {
    "package_id": "dev.ternilo.rhai-echo",
    "version": "1.0.0",
    "settings": {
      "prefix": "Rhai: ",
      "label": "inspection"
    }
  }
}
```

WASM Component 采用同一个 manifest/bundle 契约中的另一种 `runtime.kind`，不是与 Rhai 同时存在于一个 package。CLI 的 `componentize` 用于构建 WASM payload；`verify` 会对 Rhai 和 WASM 两种 runtime 分别执行真实编译校验。
