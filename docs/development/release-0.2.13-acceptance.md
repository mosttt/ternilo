# v0.2.13 发布验收

2026-10-13。[v0.2.13 已公开发布](https://github.com/mosttt/ternilo/releases/tag/v0.2.13)。本版包括角色模型与协作模式、工具及上下文优化、项目交付工具，以及历史补齐时的阅读位置修复。`ternilo-work` 保留框架，原资源迁移工作树未纳入。

[PR #3](https://github.com/mosttt/ternilo/pull/3) 的[最终检查](https://github.com/mosttt/ternilo/actions/runs/38267888786)全部通过，合并提交与发行标签均固定为 `eeda9e8d7b5caf2301d60b4cfacaa57c8b3df826`。21 个 workspace 包、Cargo.lock、Desktop、部署示例及中英文发行说明统一为 0.2.13。[正式发行流程](https://github.com/mosttt/ternilo/actions/runs/38273523746)的第二次尝试成功完成，标签未移动。完整数据见[结构化验收结果](benchmarks/2026-10-13/release-0.2.13-public.json)。

历史位置修复在 DOM 更新前记录触底状态或当前可见段落，历史插入后恢复位置，并纳入迟到的用量栏及像素舍入偏差。修复前已复现第一批历史将触底位置推离 326 px；修复后，长单轮与 450 轮对话、1280／390／320 px、触底与上滑阅读共 12 个场景通过。每批历史响应到达后均检查位置偏移小于 2 px，并验证窄屏没有横向溢出。

本地与 CI 验证已完成：

- 前端 160 个测试文件、1039 项单元测试、7 项富文本测试、类型及三语检查通过；文档截图预览完整适配后的 10 项附件测试通过。
- Builtins 原 173 项及新增 Skill YAML 兼容用例、Protocol 38 项测试通过；最终 CI 的全 workspace Rust、SDK、受限 PostgreSQL 和平台检查通过，受影响执行包全目标 Clippy 通过。
- Local 中英韩、Server Node／Worker 的角色模型、独立推理强度、权限、用量、AST 与文档交付合同通过。另通过 6 项实时滚动、30005 条事件历史、重连与离线回归。截图检查确认文档预览不再裁掉页首。
- 多登录源、跨 Server 共享通知与撤权、真实 WSS 握手与不可信证书拒绝、PostgreSQL 备份恢复，以及解包程序的完整备份与 SQLite 快照恢复均通过。
- Worker 在隔离环境中使用可用浏览器完成实际渲染，截图同步 Server 后，停止 Worker 仍能从页面读取。GitHub 操作在没有外网的隔离 Worker 中明确报出限制，使用方式保留在正式指南中。

正式发行流程的 Linux x86_64 artifact `11759319019` 已独立下载，其 ZIP 摘要为 `sha256:b89a2ce0591e5bc4376d3e45602d9fcb598702e160edfc2f46c682a4a282cd1f`。解包后核对归档校验文件、版本元数据和实际 `--version`，再使用优化构建的 CLI／Server 执行上下文交接、实时滚动、历史加载与位置、协作模式、平台协作、本机交付与平台交付共 8 个浏览器测试文件，11 项全部通过，页面、控制台及网络错误为空。其中 Worker 为同源码树的 debug 构建。

公开下载的 CLI 与 Server 归档和上述实际浏览器验收程序完全一致，SHA-256 分别为：

- `ternilo-0.2.13-x86_64-unknown-linux-gnu.tar.gz`：`55a12fd70c835d22ca3dd24b8fcfd6276825028c3e085df194d72d1aaeead9d6`。
- `ternilo-server-0.2.13-x86_64-unknown-linux-gnu.tar.gz`：`a54e76c9394a360ca8305fd2ab1f590a98c25bb91c4f1bc7142a7c3d01bae1b9`。

42 个公开附件均通过匿名地址完整下载，大小、GitHub digest、`SHA256SUMS` 和 12 个独立归档校验文件全部一致。六目标的 12 个 CLI／Server 归档通过程序清单、原生可执行文件头、版本与路径检查；每个归档内的 109 份正式文档与标签内容一致。`SOURCE` 的版本、提交及 Linorun pin 均匹配。9 个安装器通过格式检查，六目标的原生 Desktop 后台服务生命周期在 CI 中通过；这些检查不代替完整的实机安装／卸载验收。

公开镜像 `ghcr.io/mosttt/ternilo-server:0.2.13` 与 `latest` 的索引均为 `sha256:0ce9f33f929b855e0cddc2a2ce45072b7c16a2236cfd45b860434ed2205fc3d7`。匿名读取确认 AMD64／ARM64 清单、版本、源码提交、非 root 用户及发行附件中的镜像元数据一致。重新拉取公开 AMD64 镜像后，实际验证非 root 进程、只读根目录、初始化、登录、设置、内嵌网页与重启后数据保留；ARM64 在原生 CI 上验证同一候选镜像，发布步骤另核对加载后的镜像 ID。

收尾处理包括：Skill 前置信息改用只启用反序列化的 `serde-saphyr`，多行文本、中英韩、字段别名及布尔形式兼容检查通过，移除停止维护的 YAML 依赖；依赖公告、许可证和来源审计通过，npm audit 为 0 漏洞。PostgreSQL 隐私合同的完整导出超过 Node 默认 1 MiB 缓冲，测试 helper 调整为 16 MiB，保留全部真实路径、凭据与令牌断言。平台角色浏览器合同明确使用 `zh-CN` 定位登录框，完整重跑通过。

首轮 macOS Intel 打包任务达到 120 分钟上限被取消，GitHub 注释明确为 `The job has exceeded the maximum execution time of 2h0m0s`。保留已通过产物，仅重跑该目标及后续发布步骤，同一标签最终通过。后续 macOS Intel 打包时限调整为 180 分钟，其他平台仍为 120 分钟；工作流通过 actionlint 1.7.12 检查。这一流程维护变更不移动发行标签。临时公开产物校验器同时兼容 Windows SHA-256 文件的标准二进制标记，重新核对后全部通过。

完整实机安装／卸载、Desktop 发行者签名、macOS 公证及生产自动更新仍沿用原计划，未扩大本版的验收结论。本机临时验证工具与环境配置未写入项目标准测试流程。
