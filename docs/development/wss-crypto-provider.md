# WSS 启动崩溃修复

发行版 v0.2.3 的 Node 在开始 WSS 握手时会因为 Rustls 同时启用 Ring 与 AWS-LC 而 panic。HTTPS 清理通道使用 Reqwest 自选的后端，随后 WebSocket 构造默认 TLS 配置时无法自动决定进程级后端。问题发生在 WebSocket 认证之前。

客户端、Server、Worker、Desktop 的启动入口现在都显式安装 Ring 作为进程级默认后端。证书和主机名校验继续启用，没有新增不安全开关。

实际回归使用独立 Node 进程和本机临时 CA：HTTPS 清理成功后进入 WSS 握手，WebSocket 必须拒绝其未信任的证书，服务必须保持运行。真实 v0.2.3 发行程序在该用例中复现同一 CryptoProvider panic；修复版通过。CLI 构建及客户端／公共本地库全目标 Clippy 通过。回归用例加入 Linux 检查流程。

验证没有使用用户的 Node 凭据，没有连接或改动生产实例。

2026-10-05：修复已随 [v0.2.6](https://github.com/mosttt/ternilo/releases/tag/v0.2.6) 公开。实际发行 `ternilo 0.2.6` 通过上述握手回归，公开归档 SHA256 与已测程序完全一致；完整发行门禁也通过该合同。公开 Server 双架构镜像及 `latest` 已验证，见[发行验收](release-0.2.6-acceptance.md)。
