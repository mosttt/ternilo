# CI 与客户端／Server 闭环进度

状态：进行中。此文件记录本轮开发事实与未完成项；正式支持范围以[产品说明](../product.md)为准。

## 本轮目标

先补齐 Apache-2.0 许可证与交付物许可文件，验证并启用 GitHub CI，建立首次 Git 提交。随后按真实代码、测试和运行结果梳理客户端与 Server 的闭环缺口。Work 保持独立的设计预留，不作为本机或远程访问的前置依赖。

## 已核实

- 仓库此前没有提交，远端 `mosttt/ternilo` 为空；Linorun 固定提交已存在于其远端，且与本机干净检出一致。
- Local Web、CLI、Node 共用 `LocalApplication`；Server 使用统一工作区路由访问本机 Node 或可选 Worker。
- 原生账号、OIDC、电脑登记、流式任务、共享权限、模型网关与两库恢复已有实现及对应测试。
- 已有 Checks、Build packages、Release 工作流，但缺少它们引用的 LICENSE、THIRD_PARTY_NOTICES.md 和 licenses，桌面编译及归档因此失败。
- 已补 Apache-2.0 正文，统一 workspace、示例、SDK、Web、桌面包与镜像声明，并补齐 UI／Windows 沙箱的第三方声明。

## 验证记录

验证进行中，最终结果与 GitHub 运行链接将在完成后补充。

## Work 预留

沿用[Work 资源与执行边界](execution-coordination.md)：持久 Work ID 与容器 ID 分离；复用版本化 executor、工作区路由、会话与模型授权；容器生命周期由宿主节点管理。当前不新增 Docker 调度、Work 数据表、独立程序或空页面。
