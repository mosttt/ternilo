# v0.2.28 发行验收

本版首次公开模型连接与工具可靠性改进，实现由 [PR #17](https://github.com/mosttt/ternilo/pull/17) 合入；范围见[模型转发](model-gateway-reliability.md)、[文件与检索](file-and-history-contracts.md)和[工具失败与只读验证](tool-execution-reliability.md)。[PR #18](https://github.com/mosttt/ternilo/pull/18) 补齐插件保存验收的界面完成条件。v0.2.27 未公开，标签保留，失败与确定性复现见[候选记录](release-0.2.27-acceptance.md)。

## 实现与候选验证

模型转发增加首次空白响应及 15 秒保活，关闭浏览器订阅不终止已接受的电脑任务；实际模型连接失效、Server 停机和撤权分别记录。请求 ID 贯穿错误详情与用量记录，缺失用量不记为零，也不自动重发结果未知的请求。标准 OpenAI Responses 显式保留可选工具参数。空文件、缺失父目录和混合语言历史检索已补齐端到端合同。

只读 reviewer 可调用预先配置的具名检查，不扩大任意 Shell 权限；临时目录与执行租约一起回收，macOS 系统工具仅获得用户临时目录内的精确 xcrun 缓存写权限。Rhai `try_tool` 保留结构化失败及超时前输出，图谱默认精简且支持 `detail: "full"`。中英文使用指南和中英韩界面同步。Salvo 1.0.1 与修复后的 zstd 依赖通过原审计策略，未新增公告忽略。

保存响应延迟的确定性诊断在旧等待条件下复现字段消失，补齐界面完成条件后通过。产品保存行为、权限、断言和等待上限不变，诊断延迟不进入标准流程。v0.2.28 客户端／Server 匹配构建通过，本机四项真实浏览器验收共 119.04 秒；TypeScript、Web 构建、243 份 Markdown、两项版本元数据及 locked Cargo metadata 检查通过。

## 正式发行与原生检查

[v0.2.28](https://github.com/mosttt/ternilo/releases/tag/v0.2.28) 于 2026-10-27 07:25（北京时间）公开。标签与来源清单对应合并提交 `6ed41bd05b9bb6ce0d403352f4b64351b3fbe147`，合并树与已验候选完全一致。[最终候选 Checks](https://github.com/mosttt/ternilo/actions/runs/40388986093) 及[同一合并提交的 main 检查](https://github.com/mosttt/ternilo/actions/runs/40397095914) 六项全部通过，包括 Windows、Linux ARM、Intel／Apple Silicon 的原生工作区与执行合同，以及两种 macOS 的系统 Python 缓存隔离和 LaunchAgent 更新／回退。

[正式 Release](https://github.com/mosttt/ternilo/actions/runs/40397110366) 最终 20 个作业全部通过，包含六目标安装包、双架构 Server 镜像、清单签名、公开发布及 latest 提升。首轮 Linux 原生 Desktop 的两项测试在后台服务就绪后，等待深链接工作区激活失败；同一树的 PR 检查、同提交 main 检查及本机 Wayland 原生程序均通过。仅指定失败作业在新运行器复验一次，冷／热深链接及 CLI 服务复用分别为 3.54 秒和 2.18 秒，原断言与上限保留；本机原生两项共 3.99 秒。首轮原因仍未确定，保留任务 44；不据此关闭历史 Windows 任务 14 或文件面板任务 43。

Cargo 工作区、22 个锁文件工作区包、Tauri、默认镜像和维护中的打包示例统一为 0.2.28。Executor 协议 49、数据代次 1、升级签名策略不变；既有 Worker 的同版及 catalog v3 要求见[升级说明](../zh-CN/releases/0.2.28.md#升级)。

## 公开成品验收

44 项公开附件的名称、GitHub 摘要和 `SHA256SUMS` 清单对应。来源清单的版本、合并提交和固定 Linorun 提交匹配；仅用仓库公钥完成客户端清单 Ed25519 验签，六个签名目标的名称、大小、摘要与兼容策略一致。实际下载的 Linux x64 客户端／Server 归档完整性通过，程序均报告 0.2.28；每个包中的 132 份正式文档与标签源码逐字节一致，不包含开发记录。

公开程序的四项真实浏览器验收全部通过，共 103.94 秒：模型连接及关闭整个浏览器后任务继续；Local 与 Server／Node 的插件草稿、双入口保存／继承和实际 `run_check`；工作区文件预览、下载、应用入口及共享权限。模型连接单独中断后，电脑控制连接仍在线，错误详情可定位同一请求与尝试的原因；未知用量保留待核对且没有自动重发。嵌套空文件实际创建、预览和下载均为零字节，中英混合及韩文子串查询命中。中英韩、手机和桌面截图已检查，页面、控制台与 HTTP 无非预期错误。

使用公开 0.2.26 客户端、公开 0.2.28 Server 和正式签名清单，经实际 Server 更新请求驱动隔离 Supervisor 服务。新版启动人为延迟 45 秒，约 48 秒后完成安装、健康确认和重连，最终为 `completed`、版本 0.2.28；辅助程序释放操作锁，再次配置 Supervisor 更新成功。测试未注入替换辅助程序，也未操作日常服务。

GHCR `:0.2.28` 与 `:latest` 的 OCI 索引均与公开 `server-image-index.json` 一致，包含 linux/amd64 与 linux/arm64，两个 manifest 对应发行 CI 已测试并原样发布的镜像。
