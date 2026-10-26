# 开发进度与技术设计

本目录保存可复核的实现决策、验证证据和未完成工作，不进入二进制交付包。面向使用和维护的说明在[中文文档](../zh-CN/README.md)和[英文文档](../en/README.md)，构建命令在[开发指南](../zh-CN/contributing.md)。

## 当前工作与发行

- [后续任务与能力边界](remaining-task-audit.md)：资源迁移、界面验收、历史问题调查及未排期扩展的统一清单。
- [v0.2.28 验收](release-0.2.28-acceptance.md)：模型与工具可靠性改进，以及浏览器保存完成条件修正。
- [v0.2.26 验收](release-0.2.26-acceptance.md)：LaunchAgent 更新与紧凑对话问答显示修复。
- [v0.2.25 验收](release-0.2.25-acceptance.md)：慢启动更新、旧执行资源及升级状态说明。
- [资源迁移](personal-resource-migration.md)：未合入实现的选择范围、管理身份与执行身份边界。

## 会话与界面

- [有界历史](bounded-session-history.md)、[加载交互](history-loading-animation.md)、[阅读位置](history-scroll-position.md)及[分层加载](server-workbench-loading.md)。
- [会话交互](session-interactions.md)、[空白会话](blank-session-lifecycle.md)、[运行中插件保存](plugin-save-while-running.md)及[插件集中配置](plugin-settings-overview.md)。
- [运行状态与错误说明](runtime-feedback-repairs.md)、[工作台性能与布局](workbench-performance-and-layout.md)及[PR 排版与语言验收](pr-review-2026-10-11.md)。

## 模型、资源与执行

- [模型转发可靠性](model-gateway-reliability.md)、[电脑模型转发](computer-model-forwarding.md)、[托管模型委托](managed-model-delegation.md)、[用量核对](model-usage-reconciliation.md)及[Cloud 统计](cloud-session-stats.md)。
- [电脑管理](computer-management.md)、[工作区管理项目迁移](workspace-project-move.md)、[管理权交接](resource-ownership.md)及[跨 Server 路由](server-node-routing.md)。
- [工具失败与只读验证](tool-execution-reliability.md)、[文件与检索合同](file-and-history-contracts.md)、[工具上下文](tool-context-and-code-map.md)、[上下文组成](context-composition.md)、[项目交付工具](project-delivery-tools.md)及[Work 设计边界](execution-coordination.md)。

## 更新与交付

- [远程客户端更新](remote-client-updates.md)、[LaunchAgent 适配](launchagent-updates.md)、[本地浏览器启动](local-browser-launch.md)及[Windows 持久化](windows-persistence-desktop.md)。
- `release-<版本>-acceptance.md` 保存对应版本的最终发行证据；未公开候选在各自记录中明确标注。`benchmarks/` 保存这些记录引用的合成测量和验收数据。

各专题文件保留相应实现及验证边界；当前状态统一以任务清单、代码和最终发行验收为准，不使用旧阶段的等待状态推断当前功能缺失。
