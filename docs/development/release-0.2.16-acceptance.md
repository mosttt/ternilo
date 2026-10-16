# v0.2.16 发布验收

2026-10-17。[v0.2.16 已公开发布](https://github.com/mosttt/ternilo/releases/tag/v0.2.16)，完成本轮 12 项协作与上下文能力发行。[PR #6](https://github.com/mosttt/ternilo/pull/6)通过[完整检查](https://github.com/mosttt/ternilo/actions/runs/38945648723)后合入 main，标签指向合并提交 `b129cd71d0356b90adde16c44e0739d0d4348d0f`，内容与通过检查的候选一致。

本版承接 [v0.2.15 候选](release-0.2.15-acceptance.md)全部功能及安全补丁，并修复正式容器构建暴露的公式许可证输出目录缺失。本机实际容器 Web 构建和其中 1046 项单测通过，生成的公式 JS、内嵌字体 CSS 与许可证逐字节匹配源码文件。v0.2.15 没有公开 Release，既有标签保持不变。

Cargo、Desktop、部署示例和中英文发行说明统一为 0.2.16。资源迁移与恢复仍留在独立分支，`ternilo-work` 继续排除；不操作日常服务或真实数据。

[正式发行流程](https://github.com/mosttt/ternilo/actions/runs/38953580465)全部通过，GitHub Release 和 GHCR latest 均为 0.2.16。42 个公开附件已下载并通过大小、GitHub digest、SHA256SUMS 和独立归档摘要校验；12 个 CLI／Server 归档的六目标文件头、程序清单和路径正确，每个归档的 115 份正式文档与标签内容一致，9 个桌面安装器格式通过。SOURCE 的版本、源码提交和 Linorun pin 一致。

公开 Linux x86_64 CLI／Server 实际报告 0.2.16，四组真实浏览器合同全部通过：本机协作与三语设置、Node／Worker 角色模型与推理控制、本机交付流程、Node／Worker 记忆和文档证据。页面、控制台及相关请求错误为空；已查看桌面设置、韩文窄屏上下文／工具预算／推理卡片、发布状态和 Worker 公式截图。Worker 使用相同源码的本机构建，不属于公开附件。

公开镜像 `ghcr.io/mosttt/ternilo-server:0.2.16` 与 latest 索引均为 `sha256:f1a76c658900de3149d88f5b6fc4bea673c4eab6357d731388d298d4e911e7ed`。匿名读取核对 AMD64／ARM64 清单、源码提交、版本、非 root 用户与发行附件元数据。实际拉取公开 AMD64 镜像后，非 root 进程、只读根目录、初始化、登录、设置、内嵌网页和重启持久化烟测通过；ARM64 同一候选已在原生 CI 验证。

已完成的 `feat/agent-coordination`、`feat/plugin-settings-overview`、`feat/capability-completion`、0.2.13／0.2.14 验收文档分支及 0.2.15／0.2.16 发行分支均已纳入 GitHub main。两个未完成资源迁移分支和历史备份保留，不合入旧备份覆盖其已集成的后继；没有删除分支。结构化证据与分支提交见[公开发行验收](benchmarks/2026-10-17/release-0.2.16-public.json)。

安装器格式和 CI 原生服务生命周期检查不代替所有平台的完整实机安装／卸载。Desktop 签名、公证及生产更新资料沿用暂缓条件。
