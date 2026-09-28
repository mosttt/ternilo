# 项目共享继承 / Project sharing inheritance

状态：实现完成，SQLite／受限 PostgreSQL、旧库升级、相关 Rust 回归、19 项 Web 测试及真实三账号浏览器流程通过，最终文档、Clippy、i18n 与生产构建检查通过。

项目由空间管理员管理，工作区及会话仍有独立所有者。项目规则不能静默公开已有本机或托管资源，因此由工作区所有者显式开启项目共享继承；默认关闭，既有数据升级后不自动改变共享范围。项目管理员可对当前团队成员或权限组设置查看、提交、停止和配置权限，不能通过继承取得删除、转移所有权或管理工作区直接共享的权限。

启用继承的工作区及其会话、归档与同位置分叉，按当前项目规则和当前组成员关系实时计算权限。关闭继承或撤销项目规则后，派生权限消失；直接共享继续独立有效。分叉不能把项目或组规则固化为永久个人授权。个人空间不开放协作共享。项目规则不赋予机器所有权、电脑管理权限、账号模型凭据或额外预算。

复用现有分页共享编辑器、权限布尔项及审计。新增项目用户／组规则和工作区加入记录，保留外键清理与 PostgreSQL 租户隔离；不改变现有资源归属。需要同时覆盖资源列表发现、有效权限、文件读取、模型来源、归档与实时撤权，不能只在页面显示继承开关。

Projects are space-managed while workspaces/sessions have independent owners. Owners explicitly opt workspaces into project sharing; existing resources remain opted out. Project user/group policies grant current view/submit/stop/configure capabilities without deletion, ownership transfer, direct-share management, machine ownership or model-budget privileges. Inheritance follows current membership and policy, remains live for same-placement forks and archives, and disappears when revoked or opted out. Direct grants remain independent. Implement database isolation, audit, discoverability and actual browser permission workflows alongside the UI.

实现选择：保留 Control schema 14，参照 authentication／OIDC 的独立组件初始化方式新增 `project_sharing` schema 1，避免修改已有共享表的 resource_kind 约束。项目用户／组规则存独立表，工作区加入记录默认不存在；PostgreSQL 为新表配置相同租户 RLS 与受限 runtime 权限。

公开资源类型增加 project，其真实创建者来自 `control_projects.created_by`，共享管理能力由当前 ProjectManage 角色决定；新增明确的 `can_manage_sharing`，不把管理员伪装成资源所有者。现有 Workspace／Session 的共享管理仍仅允许所有者。项目规则参与工作区和会话权限并集及可发现列表；来源记录明确标识 project。分叉只保留既有直接／普通组规则，项目规则继续通过相同工作区实时继承。

界面复用共享编辑器，团队成员可查看项目共享规则，管理员编辑；工作区所有者在查看规则后显式启用。共享对象、组成员变更、历史／归档和模型配置仍复用原权限判断。真实验收包含一个管理员、另一个成员拥有的 Node、第三个共享成员，证明管理员仅管理项目规则并不自动获得未授权工作区的数据。

验证覆盖：管理员与资源所有者分离、成员／组规则、角色限幅、直接授权独立、继承开关、成员加入／退出、跨租户 RLS 与 invoker 视图、Control 14 旧库追加组件而身份／归属不变。Cloud 另验证发现、文件目录／下载、正文／标题搜索和归档撤权；真实 Node 浏览器验证成员读文件、分叉、归档、撤权后内容清空且所有者任务继续、重新启用使用当前只读规则，以及 320／390 像素布局。

浏览器早期失败来自测试定位隐藏空间切换器／单台电脑不显示机器名，以及对服务器确认型开关使用了立即检查方法。已采用可见切换器、工作区标题、保存响应与确认状态等待。直接工具命令展示工具结果，验收展开真实 read 输出。补充 Cloud 测试发现独立文件 SQL 的候选过滤遗漏继承，已补齐 SQLite／PostgreSQL 与离线来源查询，未降低权限断言。

最终 PostgreSQL 验收在受限 runtime 账号下通过 10 项契约，涵盖认证／恢复、模型服务、项目继承及文件／搜索／归档；另复验非项目创建者的空间管理员管理规则、降为普通成员后权限消失。CI 已纳入项目继承双库契约和三账号浏览器流程。
