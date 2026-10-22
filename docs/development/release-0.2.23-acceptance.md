# v0.2.23 发行验收

包含已完成的客户端远程升级、侧栏电脑下拉选择、短屏侧栏修复、Ternilo／Server 各自运行版本显示，以及 PostgreSQL 事务启动取消修复。此前功能证据见 [v0.2.22 验收](release-0.2.22-acceptance.md)。资源迁移 WIP、Work 和新版本提醒继续排除。

## 安全依赖与测试修正

v0.2.22 正式发行尚未公开时，PR #13 的依赖审计发现 DOMPurify 3.4.16 和 Vitest 4.1.11 的新公告，停止该候选发行，不重试旧依赖版本。定向升级 DOMPurify 至 3.4.17、Vitest 至 4.1.12，未使用强制批量升级。DOMPurify 用于工作区 Markdown／HTML／SVG 预览；原有 iframe 隔离、CSP、标签和 URI 限制保持。

相关公告为 DOMPurify 的 [GHSA-cg2g-xh64-v4q7](https://github.com/advisories/GHSA-cg2g-xh64-v4q7)、[GHSA-mrv2-3pqr-j5jr](https://github.com/advisories/GHSA-mrv2-3pqr-j5jr)、[GHSA-c6gr-qvv4-5v3f](https://github.com/advisories/GHSA-c6gr-qvv4-5v3f)，以及 Vitest 的 [GHSA-qxg6-89mg-rqv8](https://github.com/advisories/GHSA-qxg6-89mg-rqv8)、[GHSA-xp26-3454-mjgp](https://github.com/advisories/GHSA-xp26-3454-mjgp)。

平台模型浏览器测试等待对应提交的 `turn_finished`，再检查实际文件内容；电脑选择测试等待菜单关闭与异步焦点恢复。两项均保留原功能断言。

定向验证通过：npm audit 为 0 漏洞，富文本测试 8 项、相关组件／面板状态测试 6 项、类型检查、Web 构建、发行／签名脚本 4 项与 234 份 Markdown 检查。新 Web 资源配合隔离的 0.2.22 后端完成真实浏览器文件面板流程（12.47 秒）：Markdown／HTML 预览、脚本与外部追踪隔离、图片、下载、双栏与手机布局、工作区授权及撤销均通过，实际截图已检查；正式发行仍使用同版本程序内嵌的新资源。

## 发行状态

v0.2.20、v0.2.21、v0.2.22 标签保留为未公开候选。版本、签名策略首个客户端版本及 Server 下限统一到 0.2.23；首次仍需成套手动升级至 Executor 协议 49。生产签名私钥仅由受保护发行环境使用。

待完成：候选 CI 与合并、正式发行、公开包验签与实际运行、公开双架构镜像验收。
