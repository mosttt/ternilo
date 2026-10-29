# v0.2.29 发行验收

[v0.2.29](https://github.com/mosttt/ternilo/releases/tag/v0.2.29) 于 2026-10-29 14:02（北京时间）公开。标签与来源清单对应 [PR #20](https://github.com/mosttt/ternilo/pull/20) 的合并提交 `7347e047f890d699a4b839b83ea6b343844fd4fd`，合并树与最终已验候选一致。

## Provider 配置与完整检查

本版公开 DeepSeek 独立搜索／抓取、Responses 内置搜索和 Provider 统一网页配置。账号保存一份配置，独立网页工具在任务电脑执行；跨电脑模型来源不改变工具执行位置。网页凭据独立于模型 Key，按授权临时提供，不进入执行电脑凭据库、历史或持久命令回复。入口、权限及 Worker 边界见[网页访问](../zh-CN/web-access.md)，实现与候选排查见[技术记录](provider-web-access.md)。

[最终候选 Checks](https://github.com/mosttt/ternilo/actions/runs/40812731577) 与[合并提交的 main Checks](https://github.com/mosttt/ternilo/actions/runs/40818251049) 各六项全部通过，包括完整 Web／Rust／SDK、SQLite／受限 PostgreSQL、依赖检查、Linux ARM、Windows 和两种 macOS 原生合同。macOS LaunchAgent 更新与回退按既有门禁通过。

[正式 Release](https://github.com/mosttt/ternilo/actions/runs/40818263305) 首次运行的 20 个作业全部通过，包含完整 Checks、六目标安装包、双架构 Server 镜像、清单签名、公开发行和 latest 提升。候选阶段发现的默认模型定位、空白会话布局时序、测试 Shell 启动预算及故障注入代理连接复用问题均已处理；服务账号撤权后任务先入队再等待的实际回归通过入队前校验修复。

## 公开成品验收

44 项公开附件的名称、GitHub 摘要和 `SHA256SUMS` 清单一致。`SOURCE` 对应版本、标签合并提交及固定 Linorun 提交 `46cca18d63d1bef1c5475ac6912f18f5426a115b`。仅使用仓库公钥完成 Ed25519 验签，六个签名目标的文件名、大小、摘要和兼容策略一致。

实际下载的 Linux x64 客户端与 Server 归档完整性通过，程序均报告 0.2.29。每个归档中的 134 份正式文档与标签源码逐字节一致，不含开发记录。GHCR `:0.2.29` 和 `:latest` 的 OCI 索引均匹配公开镜像元数据，包含发行 CI 已测试的 linux/amd64 与 linux/arm64 镜像。

公开程序的浏览器与代理验收共 9 项通过，用时 140.54 秒：Provider 本机／Server Node、跨电脑来源、三语配置、插件双入口、凭据删除、402 和取消；独立 DeepSeek 两条执行路径；DeepSeek／Claude 内置工具各四条模型来源；关闭浏览器后继续运行、模型连接中断及请求记录；真实 TypeScript／Python SDK 的服务账号执行、共享与撤权。单个服务凭据撤销后，Live 在该次验收中 42 ms 内关闭，其他凭据保持可用。

Provider 流程确认模型来源电脑没有任务会话，任务电脑没有持久保存委托 Key；页面、控制台及非预期 HTTP 错误为空。已检查中英韩、手机／桌面、Provider 管理值及缺失凭据提示的实际截图。上游使用受控协议服务，不消耗真实收费账号。

## 首次升级边界

Executor 协议为 50，数据代次仍为 1；目录修订为 Builtins v22、Local v22、Cloud v5。签名清单最低客户端为 0.2.29，Server 范围为 `>=0.2.29, <0.3.0`。从协议 49 升级需等待任务结束后手动统一更新 Server 与接入端，不能通过新版 Server 直接远程升级旧协议电脑。操作步骤见[发行说明](../zh-CN/releases/0.2.29.md#升级)。
