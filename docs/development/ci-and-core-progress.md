# CI 与客户端／Server 闭环进度

[English](ci-and-core-progress.en.md) · 简体中文

状态：进行中。此文件记录本轮开发事实与未完成项；正式支持范围以[产品说明](../product.md)为准。

## 本轮目标

先补齐 Apache-2.0 许可证与交付物许可文件，验证并启用 GitHub CI，建立首次 Git 提交。随后按真实代码、测试和运行结果梳理客户端与 Server 的闭环缺口。Work 保持独立的设计预留，不作为本机或远程访问的前置依赖。

## 已核实

- 仓库此前没有提交，远端 `mosttt/ternilo` 为空；Linorun 固定提交已存在于其远端，且与本机干净检出一致。
- Local Web、CLI、Node 共用 `LocalApplication`；Server 使用统一工作区路由访问本机 Node 或可选 Worker。
- 原生账号、OIDC、电脑登记、流式任务、共享权限、模型网关与两库恢复已有实现及对应测试。
- 已有 Checks、Build packages、Release 工作流，但缺少它们引用的 LICENSE、THIRD_PARTY_NOTICES.md 和 licenses，桌面编译及归档因此失败。
- 已补 Apache-2.0 正文，统一 workspace、示例、SDK、Web、桌面包与镜像声明，并补齐 UI／Windows 沙箱的第三方声明。

## 验证记录

首次提交 `da0d9bf` 已推送到 GitHub main；[首次 Checks](https://github.com/mosttt/ternilo/actions/runs/36443218372) 已启动。远端完成情况仍待确认，不能将工作流文件存在视为全部验收通过。

首轮远端依赖门禁通过；Linux 的 932 项前端测试中，目录选择器的焦点恢复断言提前于 Radix 卸载定时器执行，导致 1 项失败。已改为等待实际焦点恢复，保留原断言。首次本机浏览器运行使用了先前编译的二进制，资源摘要和 OIDC 契约均提示其与当前源码不匹配；该轮不计入当前版本验收，重建后重新验证。

本机已通过：148 个前端测试文件／932 项单元测试、7 项富文本测试、TypeScript、i18n、包含开发记录的文档链接检查、Web 生产构建、30 项部署／发行脚本测试及运维配置验证、actionlint、Rust 依赖许可／来源／安全门禁与 npm audit。Rust 和真实浏览器验证进行中。

匹配二进制重建后，SDK 读写、5 项本地／账号／权限组／模型服务浏览器流程、6 项 OIDC／安全设置／PWA／移动布局流程，以及 2 项真实交付包和 SQLite／PostgreSQL 恢复验收均已通过。模型弹窗的视口断言暴露了 resize 事件尚未处理完的时序问题；权限组与模型服务测试现先等待应用同步视口高度，再保留原有布局与触摸尺寸断言。第二轮远端已通过 Web／部署／依赖门禁及 macOS Apple Silicon 编译，完整 Rust 和其余平台结果仍在确认。

CI 进一步加入依赖编译缓存：固定缓存 Action 提交，使用仓库工具链，并按目标平台和 Linorun 提交区分。失败的运行也保存依赖缓存，以缩短后续复验；不缓存工作区源码来替代编译验证。

## 已确认的未完成项与顺序

以下为代码与文档共同证实的缺口，不是已承诺交付的功能。不存在 `todo!` 不代表产品已经闭环。

1. **P0：首次 CI 与实际交付验证。** 确认本次 GitHub 的干净检出、四平台构建和 Linux 浏览器／恢复结果；失败优先修复。桌面签名、安装／卸载和真实平台运行验收仍须分别完成，源码编译不能替代。证据：`.github/workflows/ci.yml`、`.github/workflows/packages.yml`、[CI 说明](../ci-release.md)。
2. **P1：原生账号凭据恢复。** 目前有原生认证、OIDC 绑定、封禁恢复及登录配置重置，但没有密码找回或重置。先确定管理员丢失凭据时的本机维护恢复流程，再考虑邮箱验证和用户自助恢复；验收应保留原 user_id／资源，并使旧登录失效。证据：`apps/ternilo-server/src/admin.rs`、`platform/identity.rs`、[安全说明](../security.md)。
3. **P1：长历史读取闭环。** Live 有增量读取，但打开历史及归档仍加载完整事件快照。后续应贯穿本地存储、Node 传输、Server 路由和网页建立有界分页，验证大历史首次打开、前向补读、断线重连与归档预览。证据：`crates/ternilo-local/src/application/history.rs`、[产品限制](../product.md)。
4. **P1：模型用量完整性。** Server 已有平台／自备模型账本和预算；设备直接调用 Provider 的用量未汇总到 Server，未知用量也缺少完整的核对操作流程。后续验证离线重连去重、原操作者与预算来源、迟到用量及未知消耗保留。证据：`apps/ternilo-server/src/platform/models/gateway/tests.rs`、[模型服务](../model-service.md)。
5. **P2：协作资源生命周期。** 工作区／会话共享与权限组已实现，项目级继承、资源所有权交接及账号停用后的完整任务清理仍缺失；必须明确正在执行任务、共享撤销和文件归属的语义。证据：[产品限制](../product.md)、`crates/ternilo-control/src/account_status_store/tests.rs`。
6. **P2：远程自动化与扩容。** 当前 SDK 仅启动本机 stdio 子进程；Server HTTP／WebSocket SDK 和重连尚未提供。Server 的 Node 连接及通知仍在单进程内，多副本必须先实现连接持有者路由和通知同步。证据：[自动化](../automation.md)、`apps/ternilo-server/src/platform/edge/connection.rs`、[部署边界](../deployment.md)。

本轮新增 CI 覆盖 Local／内置工具／授权／RPC／ACP、真实 Python／TypeScript SDK、权限组与独立模型服务浏览器流程，以及 PostgreSQL Node 路由。后续功能优先服务单个 Server 管理本地机器和 VPS，不以 Work 或跨 Server 扩容作为前置工作。

## Work 预留

沿用[Work 资源与执行边界](execution-coordination.md)：持久 Work ID 与容器 ID 分离；复用版本化 executor、工作区路由、会话与模型授权；容器生命周期由宿主节点管理。当前不新增 Docker 调度、Work 数据表、独立程序或空页面。
