# 密钥保存位置

Server 的数据库连接和主密钥保存在私有数据卷中的 `server.json`。平台模型的上游 API Key 加密保存在数据库里；在“平台管理 → 模型服务”中保存后即可生效，不需要修改 Worker 配置或重启服务。

自动化配置可使用 `ternilo-deploy configure-model --provider-profile provider.json`。`provider.json` 只放 Provider 配置，API Key 通过 `TERNILO_MODEL_API_KEY` 或隐藏输入提供；操作需要平台管理权限。此命令通过 Server 管理接口保存，不生成独立的明文模型密钥文件。

模型 Key 轮换由 Server 暂存加密的旧值。`cloud-rotate-credentials.sh model-key` 使用指定 Provider 的新 Key 执行验证命令，验证失败就恢复旧值，成功后确认轮换。中断后可以用 `model-key-status` 查看状态，再使用确切的轮换 ID 提交或回滚，无需重新提供旧 Key。

Worker 在自己的私有数据卷中保存 `worker.json`，其中只有专属接入凭据和执行配置。它不需要数据库密码、Server 主密钥或模型 API Key。自己的电脑和 VPS 在“用户设置 → 我的机器”中接入；自备模型在用户模型设置中配置。

运行凭据和备份不要提交到仓库。恢复 Server 时，数据库与原主密钥必须来自同一份匹配备份。完整操作见[部署指南](../../../docs/deployment.md)和[Worker 部署](../../../docs/worker.md)。
