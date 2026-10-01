# 跨 Server 共享权限通知

状态：已随 v0.1.3 发布，SQLite、受限 PostgreSQL 与双 Server 浏览器验收通过。

## 当前实现

共享用户、权限组、项目规则、工作区继承和空间成员变更，与 `cloud_live_changes` 中的空间失效提示在同一事务提交。SQLite 使用已有持久补读，PostgreSQL 结合 LISTEN 与补读。提示只有空间 ID，不包含成员列表、权限明细、文件或会话内容。

每台 Server 的 Live 连接收到本空间提示后重新验证当前身份、空间成员关系和活动订阅，再从权威存储读取工作台。事务回滚不会发布提示。其他空间的通知不触发当前工作台更新。已有本机即时通知保留，跨进程传递不依赖它。

独立 `resource_live` schema 1 安装触发器。PostgreSQL 使用固定 search_path 的触发器函数写入已有变更表；runtime 保持现有表权限，不能直接调用该函数。

## 验证

- SQLite 与受限 PostgreSQL：两个独立连接池都收到提交后的授权与撤权，未提交／回滚变更保持安静，重新读取的实际权限与通知一致。
- 双 Server 浏览器验收通过：同一数据库，Node 和浏览器连接 Server B，所有共享变更发往 Server A。覆盖授权出现、活动历史清除、后续事件停止、权限组移除与项目继承变更，核对了页面渲染、控制台和撤权后的预期拒绝响应。
- 此模块负责共享权限提示。Node 的连接持有者转发及事件补读由[跨 Server 路由](server-node-routing.md)实现，浏览器联调状态单独记录。

## 资源管理权交接

`node_resources.rs` 将电脑所有者用于发现绑定校验，Cloud 的会话 `user_id` 参与执行身份和历史运行校验。[资源管理权交接](resource-ownership.md)使用独立管理记录，`placement_store.rs` 等存储读取继续使用原存储身份。

交接在同团队内核验当前管理者、可写接收者及管理版本，保留历史作者、用量、Node 凭据和数据位置。发现、改名、共享、分叉、删除和订阅通过统一权限解析；`resource_ownership_live` 在事务内发出权限失效提示。SQLite／HTTP 回归通过，受限 PostgreSQL 与真实浏览器验收仍待环境允许后完成。
