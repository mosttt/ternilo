# v0.2.15 发布验收

2026-10-16。用户要求先发布已完成的 12 项能力，再合并其余已完成分支。发行从 `feat/capability-completion` 的 `3e74470` 准备，只包含已验收的能力补齐；资源迁移分支的后续提交不进入本版。

本版增量为上下文组成、按工具预算、候选调用影响、GitHub 发布状态、实际自动推理控制和离线公式渲染。功能实现及本机／Node／Worker／中英韩证据沿用各功能记录；发行说明见[中文](../zh-CN/releases/0.2.15.md)和[英文](../en/releases/0.2.15.md)。Cargo、Desktop 和部署示例同步为 0.2.15。

当前准备发行检查。先将发行源码同步到 main，再从该提交打标签；公开产物、镜像、实际程序验收和已完成分支的合并状态在完成后补记。尚未公开发布。

首轮 [PR #5 检查](https://github.com/mosttt/ternilo/actions/runs/38914734255)发现 Rustls 0.23.45 和 Wasmtime 48.0.5 的新安全公告。依赖已分别定向升级至 0.23.46 和 48.0.6，并同步 Wasmtime 的 Cranelift 依赖；未增加公告忽略项。本机 `cargo deny --locked check advisories licenses sources` 已通过，继续验证补丁后的 WASM 合同、Worker 构建及 PR 检查。
