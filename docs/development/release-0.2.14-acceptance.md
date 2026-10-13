# v0.2.14 发布验收

2026-10-14。[v0.2.14 已公开发布](https://github.com/mosttt/ternilo/releases/tag/v0.2.14)。插件集中配置、原插件详情及 Agent Team 的设置同步已通过 [PR #4](https://github.com/mosttt/ternilo/pull/4) 的[最终检查](https://github.com/mosttt/ternilo/actions/runs/38451214738)，并合并到 main。发行标签 `v0.2.14` 指向合并提交 `b44b0d453392dae8568c1bc4b9102e48537546ec`；合并树与通过检查的 PR 源码树一致，Cargo、Desktop 及发行说明均为 0.2.14。详细功能与回归记录见[插件配置记录](plugin-settings-overview.md)。

本版还包含无工作台页面时的 Node 子会话模型同步、历史位置跟随和矮横屏模型菜单调整。资源迁移与恢复分支、后续发现的离线电脑界面调整均未纳入本标签；`ternilo-work` 继续只保留框架。

[正式发行流程](https://github.com/mosttt/ternilo/actions/runs/38456151726)首次尝试全部通过，GitHub Release 与 latest 均为 0.2.14。标签保持不动。42 个公开附件已全部下载，大小、GitHub digest、SHA256SUMS 和 12 份独立归档摘要一致；12 个 CLI／Server 归档的程序清单、路径、版本及六目标原生文件头符合预期，每个归档中的 111 份正式文档与标签一致，9 个安装器格式检查通过。SOURCE 的版本、源码提交和 Linorun pin 一致。

公开镜像 `ghcr.io/mosttt/ternilo-server:0.2.14` 与 latest 的索引均为 `sha256:1ed33d5114ec00bc89c6ec82761c1584e10a30ee9770c9c26b4a982bd7f8fdee`。匿名读取确认 AMD64／ARM64 清单、版本、源码提交、非 root 用户与附件镜像元数据一致；实际拉取公开 AMD64 镜像后，非 root 进程、只读根目录、初始化、登录、设置、内嵌网页和重启后数据保留烟测通过。ARM64 同一候选镜像已在原生 CI 验证。

公开下载的 CLI／Server 均实际报告 0.2.14。四组浏览器合同共 9 项全部通过：插件集中配置／详情／Agent Team 的中英韩同步（89.89 秒）、实时滚动和历史阅读位置（118.13 秒）、双电脑与账号邀请／共享／暂停（101.82 秒）、Node／Worker 角色模型与独立推理强度（36.62 秒）。最后一组包含离开工作台后的新子会话执行。页面、控制台及相关请求错误为空；截图已检查。Worker 使用同源码的本机构建。

公开 Linux x86_64 CLI 与 Server 归档 SHA-256 分别为 `73b17b6e0b0e0f905338d582e49153baa242fee5aaf1a447f5933bfc62b8f6f1` 和 `a8c255adeae962c21f93fec081267e05fd5ab96338055168c1ca604e9cdeb213`。Local 与 Server 实际提供的 app.js／app.css 摘要一致。结构化数据见[公开发行验收](benchmarks/2026-10-14/release-0.2.14-public.json)。

安装器格式与 CI 原生后台服务生命周期检查不代替完整实机安装／卸载。Desktop 签名、公证及生产更新沿用原计划。
