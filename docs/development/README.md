# 开发进度与技术设计

本目录记录开发进度、验证结果、未完成项及尚未实施的资源模型与接口约束，不进入二进制交付包。设计方案不代表已支持的功能。

- [Work 资源与执行边界](execution-coordination.md)：托管电脑、容器归属、节点绑定与现有执行接口的衔接。
- [原生账号恢复实施记录](native-account-recovery.md)：维护命令、会话撤销一致性与双库／浏览器验收。
- [CI 与客户端／Server 闭环进度](ci-and-core-progress.md)：本轮许可证、持续集成、验证结果和后续优先级。
- [剩余核心功能实施顺序](remaining-core-plan.md)：账号恢复、历史分页、用量及后续协作能力的实现边界与验收目标。

已支持的功能见[产品说明](../zh-CN/product.md)，模块职责见[架构](../zh-CN/architecture.md)，开发和验证命令见[开发指南](../zh-CN/contributing.md)。构建及验收结果保存在对应的 CI 和发行记录中。

- [有界会话历史](bounded-session-history.md)

- [未知模型用量核对](model-usage-reconciliation.md)

- [发行反馈与交付结果](release-feedback.md)：当前修复、验证和发行状态。
- [Windows 持久化与桌面入口](windows-persistence-desktop.md)：平台行为与退出边界。
- [浏览器会话详情](browser-session-details.md)：账号隔离、活动记录与来源 IP。
- [MCP 目录刷新与可选重连](mcp-refresh-reconnect.md)：通知处理、调用边界和有限重连预算。
