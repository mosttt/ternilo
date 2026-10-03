# 开发进度与技术设计

本目录记录开发进度、验证结果、未完成项及尚未实施的资源模型与接口约束，仅保留中文，不进入二进制交付包。设计方案不代表已支持的功能。

- [CI 与客户端／Server 闭环进度](ci-and-core-progress.md)：许可证、持续集成、验证结果和后续优先级。
- [剩余核心功能实施顺序](remaining-core-plan.md)：核心流程与后续协作能力的实现边界。
- [发行反馈与交付结果](release-feedback.md)：当前修复、验证和发行状态。
- [原生账号恢复](native-account-recovery.md)：维护命令、会话撤销一致性与双库／浏览器验收。
- [有界会话历史](bounded-session-history.md)：分页、实时续读及归档读取。
- [未知模型用量核对](model-usage-reconciliation.md)：用量来源、账本核对和幂等性。
- [Windows 持久化与桌面入口](windows-persistence-desktop.md)：平台行为与退出边界。
- [服务账号实施](service-accounts.md)：独立自动化身份、空间范围、凭据生命周期、资源授权与模型任务。
- [本站登录会话统一管理](oidc-session-management.md)：密码与本站 OIDC 会话的统一查看、活动详情、稳定刷新身份与撤销。
- [浏览器会话详情](browser-session-details.md)：账号隔离、活动记录与来源 IP。
- [MCP 目录刷新与可选重连](mcp-refresh-reconnect.md)：通知处理、调用边界和有限重连预算。
- [账号任务清理](account-task-cleanup.md)：托管执行授权、作者批次隔离和执行端确认。
- [Node 账号清理](node-account-cleanup.md)：离线撤销、启动屏障、选择性收尾和实际完成回执。
- [OIDC 来源与公网地址](oidc-browser-origin.md)：浏览器加密能力、回调来源与错误配置恢复。
- [跨 Server 共享权限通知](resource-notifications.md)：持久失效提示、双 Server 验收与资源交接约束。
- [当前剩余任务清单](remaining-task-audit.md)：本阶段收尾、发行、后续产品能力和 Work 预留的完整分类。
- [电脑管理闭环](computer-management.md)：本轮名称／备注、详情、暂停／恢复及保留历史的移除登记；凭据轮换后移。
- [本地服务自动打开浏览器](local-browser-launch.md)：本次启动的浏览器开关、就绪检测和失败时继续服务。
- [资源管理权交接](resource-ownership.md)：当前管理者、原执行身份、版本冲突与共享保留；真实浏览器和受限 PostgreSQL 验收通过。
- [跨 Server Node 路由](server-node-routing.md)：租约持有者转发、输入授权版本及合并变化提示；实际 TCP、受限 PostgreSQL 合同及真实双实例模型浏览器流程通过。
- [Work 资源与执行边界](execution-coordination.md)：托管电脑、容器归属、节点绑定与现有执行接口的衔接。

已支持的功能见[产品说明](../zh-CN/product.md)，模块职责见[架构](../zh-CN/architecture.md)，开发和验证命令见[开发指南](../zh-CN/contributing.md)。构建及验收结果保存在对应的 CI 和发行记录中。

2026-10-01：正在落实[工作台性能与配置目录](workbench-performance-and-layout.md)。功能优先；本阶段完成后再推进 GitHub 验收和发行。

- [电脑模型转发](computer-model-forwarding.md)：来源电脑调用、跨实例流式转发和独立设备报告。
- [托管模型委托](managed-model-delegation.md)：模型提供者、提交者和资源所有者分离及验收。
- [模型发现排查](provider-model-discovery.md)：Claude 官方合同与安全错误提示。
