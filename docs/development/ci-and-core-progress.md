# CI 与客户端／Server 闭环进度

[English](ci-and-core-progress.en.md) · 简体中文

状态：两轮完整 CI 与四平台打包通过；首个发布标签 `v0.1.0` 已固定在 `0ce6ba4` 并推送。该提交的检查全部通过，当前仅 Intel macOS 打包仍在运行；[Release 工作流](https://github.com/mosttt/ternilo/actions/runs/36487885237)正在验证并生成实际产物，尚未生成 Release。远程 SDK 已合入，项目共享通过本机最终验收；这两项后续开发不在首个标签快照中。

## 本轮目标

先补齐 Apache-2.0 许可证与交付物许可文件，验证并启用 GitHub CI，建立首次 Git 提交。随后按真实代码、测试和运行结果梳理客户端与 Server 的闭环缺口。Work 保持独立的设计预留，不作为本机或远程访问的前置依赖。

## 已核实

- 仓库此前没有提交，远端 `mosttt/ternilo` 为空；Linorun 固定提交已存在于其远端，且与本机干净检出一致。
- Local Web、CLI、Node 共用 `LocalApplication`；Server 使用统一工作区路由访问本机 Node 或可选 Worker。
- 原生账号、OIDC、电脑登记、流式任务、共享权限、模型网关与两库恢复已有实现及对应测试。
- 已有 Checks、Build packages、Release 工作流，但缺少它们引用的 LICENSE、THIRD_PARTY_NOTICES.md 和 licenses，桌面编译及归档因此失败。
- 已补 Apache-2.0 正文，统一 workspace、示例、SDK、Web、桌面包与镜像声明，并补齐 UI／Windows 沙箱的第三方声明。

## 验证记录

代码提交 `12c1e85` 的 [Checks](https://github.com/mosttt/ternilo/actions/runs/36446651439) 最终通过依赖门禁、Windows 与两种 macOS 检查，但 Linux 两项搜索测试失败，后续打包被跳过。日志表明 runner 无法启动 `rg`；已补充 Linux CI 的 `ripgrep` 安装与版本检查，同时补齐 `.deb` 的运行依赖及各交付形式的安装说明。此前两轮在修复提交推送后已主动终止。当前没有版本标签或 GitHub Release，需先完成检查和实际打包。

后续执行顺序：完成 CI 与可下载交付物，再依次实现下文 P1／P2 剩余项；继续优先客户端与 Server，Work 保持必要预留。

`ripgrep` 修复提交 `76801de` 的 [Checks](https://github.com/mosttt/ternilo/actions/runs/36451490404) 已通过 Linux 核心／SDK／PostgreSQL／浏览器／原生桌面／恢复验收、Windows 和两种 macOS 检查及依赖门禁，正在生成四平台发行包。后续功能的具体约束见[实施计划](remaining-core-plan.md)。账号恢复 `6d00781` 和历史分页 `72b3530` 已合入 main。恢复包含密码校验与会话签发竞态修复、旧凭据失效后的网页重新登录；分页贯穿 Local、Node、Server、实时续接及归档预览。详见[账号恢复](native-account-recovery.md)与[有界历史](bounded-session-history.md)的双库／浏览器证据。

首轮远端依赖门禁通过；Linux 的 932 项前端测试中，目录选择器的焦点恢复断言提前于 Radix 卸载定时器执行，导致 1 项失败。已改为等待实际焦点恢复，保留原断言。首次本机浏览器运行使用了先前编译的二进制，资源摘要和 OIDC 契约均提示其与当前源码不匹配；该轮不计入当前版本验收，重建后重新验证。

本机已通过：148 个前端测试文件／932 项单元测试、7 项富文本测试、TypeScript、i18n、包含开发记录的文档链接检查、Web 生产构建、30 项部署／发行脚本测试及运维配置验证、actionlint、全工作区 Clippy、Rust 依赖许可／来源／安全门禁与 npm audit。客户端、Server、Local、Builtins、Authorization、Automation、ACP、Control、Protocol、Kernel、Transport、Transport Store、Storage 与插件 CLI 的 Rust 测试及文档测试全部通过。

匹配二进制重建后，SDK 读写、5 项本地／账号／权限组／模型服务浏览器流程、6 项 OIDC／安全设置／PWA／移动布局流程，以及 2 项真实交付包和 SQLite／PostgreSQL 恢复验收均已通过。另有 3 项使用受限 PostgreSQL runtime 授权的认证设置、OIDC 会话和 Node 路由测试通过。模型弹窗的视口断言暴露了 resize 事件尚未处理完的时序问题；权限组与模型服务测试现先等待应用同步视口高度，再保留原有布局与触摸尺寸断言。第二轮远端已通过 Web／部署／依赖门禁及 macOS Apple Silicon 编译，最新提交的完整矩阵仍在确认。

CI 进一步加入依赖编译缓存：固定缓存 Action 提交，使用仓库工具链，并按目标平台和 Linorun 提交区分。失败的运行也保存依赖缓存，以缩短后续复验；不缓存工作区源码来替代编译验证。

本机最后完成了包含 `tauri/custom-protocol` 的客户端、Server、插件 CLI 和桌面构建；两项真实原生桌面测试通过，覆盖冷／热深链接、单实例转发及复用 CLI 服务。最新 GitHub 运行已通过依赖门禁与 macOS Apple Silicon 编译，并实际保存编译缓存；Linux、Windows、macOS Intel 及后续打包仍应以当前运行结果为准。此验收记录的后续纯文档提交不重复触发完整构建，代码验证对应 `12c1e85`。

包含恢复与分页的提交 `b177e44` 已推送，[新一轮 Checks](https://github.com/mosttt/ternilo/actions/runs/36463338780) 正在验证。原始基线的 [Checks](https://github.com/mosttt/ternilo/actions/runs/36451490404) 已于 2026-09-29 完成，所有任务均成功。

GitHub Linux 实际交付物已下载并通过全部 SHA256 校验；Apache／第三方许可文件齐全，`.deb` 声明 `bubblewrap` 与 `ripgrep`。使用该次 CI 的真实客户端、Server、插件二进制，完整目录／SQLite 快照与 PostgreSQL 恢复浏览器验收均通过。

## 已确认的未完成项与顺序

以下为代码与文档共同证实的缺口，不是已承诺交付的功能。不存在 `todo!` 不代表产品已经闭环。

1. **P0：首次 CI 与实际交付验证。** 确认本次 GitHub 的干净检出、四平台构建和 Linux 浏览器／恢复结果；失败优先修复。桌面签名、安装／卸载和真实平台运行验收仍须分别完成，源码编译不能替代。证据：`.github/workflows/ci.yml`、`.github/workflows/packages.yml`、[CI 说明](../ci-release.md)。
2. **已完成：原生账号凭据恢复。** 本机维护命令保留身份和资源，撤销旧原生／已绑定 OIDC 站内会话；双库及真实浏览器验收通过。使用方式见[账号恢复](../account-recovery.md)。邮箱自助恢复仍属后续能力。
3. **已完成：长历史读取闭环。** 默认读取最近 200 条，按游标补读较早事件，实时游标保持独立；归档共享／撤销／恢复及离线缓存均已验证。Cloud 派生统计仍需完整回放。见[实施记录](bounded-session-history.md)。
4. **已完成：模型用量完整性。** [未知用量核对](model-usage-reconciliation.md)已完成，Owner／Admin 可按上游记录补齐缺失计数并保留不可变审计，双库／真实浏览器验收通过。[设备 Provider 报告](device-provider-usage.md)独立于 Server 预算展示，双库与真实浏览器已验证离线补传／去重、分叉、共享提交者、权限及未知计数。证据：`apps/ternilo-server/src/platform/models/gateway/tests.rs`、[模型服务](../model-service.md)。
5. **P2：协作资源生命周期。** 工作区／会话共享与权限组已实现，[项目共享继承](project-sharing.md)通过双库和真实浏览器验收。资源所有权交接及账号停用后的完整任务清理仍缺失；必须明确正在执行任务、共享撤销和文件归属的语义。证据：[产品限制](../product.md)、`crates/ternilo-control/src/account_status_store/tests.rs`。
6. **P2：远程自动化与扩容。** 本地 stdio SDK 已有；[远程 SDK](remote-server-sdks.md)新增 HTTP／Live、分页和断线重连，真实 Server／Node 验收已通过并合入 main。Server 的 Node 连接及通知仍在单进程内，多副本必须先实现连接持有者路由和通知同步。证据：[自动化](../automation.md)、`apps/ternilo-server/src/platform/edge/connection.rs`、[部署边界](../deployment.md)。

本轮新增 CI 覆盖 Local／内置工具／授权／RPC／ACP、真实 Python／TypeScript SDK、权限组与独立模型服务浏览器流程，以及 PostgreSQL Node 路由。后续功能优先服务单个 Server 管理本地机器和 VPS，不以 Work 或跨 Server 扩容作为前置工作。

## Work 预留

沿用[Work 资源与执行边界](execution-coordination.md)：持久 Work ID 与容器 ID 分离；复用版本化 executor、工作区路由、会话与模型授权；容器生命周期由宿主节点管理。当前不新增 Docker 调度、Work 数据表、独立程序或空页面。


后续 CI 排查：`b177e44` 的 Rust／跨平台／认证门禁通过，但账号共享浏览器测试在预期撤权响应的控制台断言处失败。测试原先只确认队列的拒绝响应，遗漏了撤权时尚在完成的 commands、catalog 和 model-options 读取。现在授权编辑场景保持在管理页，主动聊天撤权场景逐一核验相同 Session 的完整 400 错误体，只有已核验的对应控制台项被识别为预期；实时流停止、界面清空及所有者任务继续的原断言保留。真实浏览器复验通过。包含用量核对的 `40c2365` 正在进行 [Checks](https://github.com/mosttt/ternilo/actions/runs/36466395897)，随后由修复后的 main 继续验证。

最新 CI 的 `8215c9` 在 i18n 门禁发现已移除占位页的 `model-service.localUsageUnavailable` 遗留键；已删除中英文两处键，翻译检查与生产构建通过。设备用量另已通过 119 项 Server 测试（3 项既有环境忽略）、401 项受影响 Rust 库测试、双库合同和真实浏览器。原生模型连接通过 3 项单元测试、Clippy，以及 Gemini／Anthropic 的平台与私有来源真实工具调用流程。

`29d30f0` 的全部 Rust、跨平台、依赖、SDK 与受限 PostgreSQL 检查通过；浏览器阶段在共享撤权场景失败，原因是 UI 取消读取后 Chromium 已释放 response body，测试异步读取正文发生竞态。改为在真实 HTTP 响应交给浏览器前读取并校验完整拒绝正文，再原样交付；结束时等待路由处理完成。仅确认目标会话的 400／403 标准错误，保留事件停止、内容清空和所有者任务继续断言。最终真实浏览器复验通过，共校验 8 次拒绝读取。
