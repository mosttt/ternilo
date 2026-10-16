# v0.2.15 发布验收

2026-10-16。用户要求先发布已完成的 12 项能力，再合并其余已完成分支。发行从 `feat/capability-completion` 的 `3e74470` 准备，只包含已验收的能力补齐；资源迁移分支的后续提交不进入本版。

本版增量为上下文组成、按工具预算、候选调用影响、GitHub 发布状态、实际自动推理控制和离线公式渲染。功能实现及本机／Node／Worker／中英韩证据沿用各功能记录；发行说明见[中文](../zh-CN/releases/0.2.15.md)和[英文](../en/releases/0.2.15.md)。Cargo、Desktop 和部署示例同步为 0.2.15。

最终 [PR 检查](https://github.com/mosttt/ternilo/actions/runs/38938587845)全部通过，[PR #5](https://github.com/mosttt/ternilo/pull/5)已合入 main。`v0.2.15` 指向合并提交 `b447fd79348b49dcca3f87de73085df24e8c0e27`，其内容与通过检查的候选一致。

2026-10-17，[正式发行构建](https://github.com/mosttt/ternilo/actions/runs/38944894507)在容器 Web 构建阶段失败：公式资源脚本未创建许可证输出目录，写入 `licenses/katex.MIT` 时得到 ENOENT。该版没有公开 Release；停止其剩余构建，保留既有标签，不覆盖。修复后改由 [v0.2.16](release-0.2.16-acceptance.md)完成本批发行。

首轮 [PR #5 检查](https://github.com/mosttt/ternilo/actions/runs/38914734255)发现 Rustls 0.23.45 和 Wasmtime 48.0.5 的新安全公告。依赖已分别定向升级至 0.23.46 和 48.0.6，并同步 Wasmtime 的 Cranelift 依赖；未增加公告忽略项。本机 `cargo deny --locked check advisories licenses sources`、5 项 WASM 合同及 Worker 构建通过；[补丁后的 CI](https://github.com/mosttt/ternilo/actions/runs/38917013948)安全审计、四平台检查、Rust 检查、SDK 和受限 PostgreSQL 合同也通过。

该次 CI 在自动推理卡片的浏览器可见性检查失败。定位为测试在展开过程后立即读取“回到底部”按钮，早于 ResizeObserver 更新；错过按钮后便一直等待卡片自行移入视口。测试改为先等待卡片可见或导航按钮出现，再点击并检查卡片完整位于输入区上方，保留几何断言并增加失败截图。修复后的中英韩实际浏览器合同及最终 CI 通过，页面、控制台和相关请求错误为空。

下一轮 macOS ARM 检查暴露了旧测试目录命名的并发碰撞：两个 fork 测试仅以进程号与系统时钟命名，在同一时钟刻度创建了同一个工作区目录。共享测试辅助函数增加进程内原子序号，保持原有“返回尚未创建的路径”约定；未改动产品代码或改为吞掉目录已存在错误。3 项 fork 合同本机通过，最终 macOS 原生 CI 通过。

[后续 CI](https://github.com/mosttt/ternilo/actions/runs/38928589841)四平台及安全检查通过，PWA 合同在模拟恢复网络后的重载超时。排查确认原测试的异步 `waitForFunction` 即使返回 false 也会结束，未真正等待网络；其探测的 `/assets/boot.js` 在本地服务也不存在。改用已有异步轮询辅助函数和实际未缓存的资源，分别等待页面与控制页面的 Service Worker 网络恢复，并在离线阶段前等候更新后的 Worker 接管。缓存清理同样改为等待实际结果；完整 PWA 安装、更新、离线、恢复与响应式合同通过。未修改产品的离线缓存或恢复策略。

PWA 修复后的 CI 通过上述阶段，随后预设开关尺寸断言采到了对话框 95% 入场缩放中的 38.225px，而非最终 40px 点击区域。测试在测量前等待实际对话框动画结束；保留尺寸、键盘操作、继承和窄屏溢出断言，本机完整合同通过。
