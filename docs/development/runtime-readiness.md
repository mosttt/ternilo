# 本地工具可用性与运行反馈

本次补丁修复默认文件工具依赖外部 `rg`、Windows shell 输出乱码，以及沙箱依赖缺失直到调用时才暴露的问题。与已提交的工作区图片展示修复一起发布；资源迁移工作保持暂停，不包含在此补丁中。

## 已实现

文件匹配使用 `ignore`，正则与逐行搜索使用 `grep-regex`、`grep-matcher` 和 `grep-searcher`。保留隐藏文件、忽略规则、显式 glob 覆盖、单行首个匹配的字节列号及部分结果诊断。结果数量、有界输出、长行内存和十秒超时仍受限制；取消请求会通知阻塞搜索退出。符号链接不跟随。

Windows 输出在需要时用同宿主的只读 PowerShell 探针取得输出代码页，通过成熟解码库处理中文、韩文和 OEM 编码；UTF-8 和 UTF-16 BOM 优先。原命令、退出码和系统限制保持原样。shell 非零退出和超时标记为工具失败，界面仍可查看捕获的 stdout／stderr，并按 PowerShell 错误标识说明原因。

默认本地沙箱检查随现有工作区信息请求路由到执行电脑，不添加新的 Node RPC。检查使用真实沙箱运行有界空命令，区分缺失与无法启动。会话提前显示中英韩修复说明、已识别 Linux 发行版的安装命令和重新检查入口。旧执行端、自定义沙箱和云端 Worker 未提供这一可选字段时，不显示“检查通过”。Worker 继续使用自身已有隔离与接入检查。

## 验证结果

内置搜索及 shell 生命周期相关的 12 项测试已通过，覆盖忽略规则、中文列号、UTF-16 文件、错误表达式、结果上限、权限拒绝、外部符号链接、长行和取消。Windows 输出解码覆盖 CP936、CP949、CP437、UTF-8 与 UTF-16 BOM。

Windows 原生 PowerShell 与 ConstrainedLanguage、Linux ARM64、macOS ARM64／Intel 的最终平台 CI 均已通过。最终候选的严格 Clippy 通过。UI 已覆盖失败命令原始输出、中英韩沙箱修复、检查中的反馈，以及跨会话清除旧提示。

真实 Server／Node 首轮验收已确认：受限 PATH 无 `rg` 时文件搜索成功；失效沙箱未执行待写入命令；安装替换后真实沙箱自检恢复。浏览器检查还补齐空白会话首次展示与窄屏布局。完整 Server／Node 浏览器验收已通过（126.00 秒），包含既有权限、离线历史、图片和文件交付回归；补充可视检查确认 320px 下的修复按钮和错误说明可见，原始 stderr 保留。证据见 [runtime-readiness.json](benchmarks/2026-10-21/runtime-readiness.json)。

验证中发现 Windows 首次输出代码页探针可能超过原 3 秒上限。探针和 Windows 沙箱首次自检改为有界的 10 秒后，Windows 原生测试通过。最终可视验收还确认：诊断说明使用 14px 文本，在窄屏上可阅读，重试按钮可见。

## 发版依赖修复

CI 的依赖门禁发现 Wasmtime 48.0.6 受到 RUSTSEC-2026-0378 和 RUSTSEC-2026-0379 影响。上游分别说明 component stream/future 类型混用可触发宿主 panic，以及 AArch64 大型 br_table 编译分支可能跳往错误目标。采用同系列修复版本 48.0.7，不新增 advisory 忽略项。35 项扩展运行测试以及最终依赖、许可和来源门禁均已通过。

## 0.2.19 已发布并完成公开产物验收

[PR #9](https://github.com/mosttt/ternilo/pull/9) 已合并到 GitHub `main`，随后从合并提交 `d3cbf75bc7790d2f0cbceb6d842715fac15f3414` 创建标签。[v0.2.19 Release](https://github.com/mosttt/ternilo/releases/tag/v0.2.19) 已正式发布；源码、Cargo、Desktop 与公开程序的版本号一致。

[合并前 CI](https://github.com/mosttt/ternilo/actions/runs/39465406076) 的 6 个任务与[发布流水线](https://github.com/mosttt/ternilo/actions/runs/39473943820) 的 19 个任务全部成功。发布包含 42 个附件；公开 Linux x86_64 客户端／Server 和 Windows x86_64／ARM64 客户端归档已校验下载完整性。两个 Windows 包均包含主程序、插件程序和沙箱辅助程序，原生架构匹配；Windows ARM64 的持久化和工作区边界验收也在发布流水线中通过。

使用实际公开下载的 Linux 客户端和 Server 再次执行完整运行反馈浏览器验收，134.15 秒通过，页面、控制台和非预期网络错误为零。已检查公开产物的工作区图片和窄屏沙箱提示截图。Server `0.2.19` 与 `latest` 均包含 Linux amd64／arm64，注册表清单与 Release 附带的已测试镜像清单一致。
