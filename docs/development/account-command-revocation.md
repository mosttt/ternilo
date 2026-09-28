# 账号停用与 Node 持久命令

状态：禁止旧输入补发已通过 SQLite／受限 PostgreSQL、真实 WebSocket 重连及 123 项 Server 回归。完整任务取消仍需后续闭环。

检查发现，封禁／注销会撤销登录、模型和本人电脑凭据，但共享成员此前提交到他人电脑的持久命令，仍可能在电脑重连时重发。只检查账号当前是否 active 也不充分：快速封禁后解封会重新放行旧输入。

为每个新接收的账号输入保存当时的账号状态版本。接收事务与账号状态操作使用同一账号锁；命令去重不能刷新旧版本。每次首次投递和补发都核对当前状态和版本，不符合时永久禁止该命令后续发送。已退出队列的其他账号任务不受影响；解封后新提交取得新版本，可正常执行。

未曾发出的 pending 命令返回明确的拒绝回执。inflight 命令可能已执行，因此仅禁止再次发送，继续保留实际回执接收和原有超时规则，不伪造“已取消”或回滚结果。早于本版本且没有接收证据的持久输入也不重放；历史事件和已经完成的回执保留。禁止重放不是运行中取消。

新增 `gateway_input_authorization` schema 1 组件与按租户隔离的附表，不修改 Control 14、Gateway 1 或 executor protocol 44。升级应先停止旧 Server，按现有方式使用 schema owner 初始化后由受限 runtime 运行。

验收覆盖 SQLite／受限 PostgreSQL、封禁期间原子拒绝接收、快速解封不复活旧命令、去重不刷新版本、单条批次不被失效命令阻塞、迟到真实回执、重新打开 journal、旧库无证据输入及真实认证 WebSocket 重连。

后续运行中清理必须沿 Server 接收输入的身份追踪队列、当前运行、持续目标和派生子任务。不能取消同会话中其他人的独立输入，也不能把 Node 断线或租约过期当成外部进程已停止。机器离线时，清理请求必须持久保留并展示待确认状态；这需要独立的执行协议和执行端收尾验收，当前改动没有宣称实现。

The journal now records the account status revision at input admission and revalidates it before initial delivery or replay. Ban/unban cannot revive earlier inputs; duplicate admission cannot refresh their authority. Undispatched commands receive denial receipts. Inflight commands retain uncertainty and actual late replies, without fabricated cancellation. Older inputs lacking admission evidence are withheld. Full cancellation across queues, active runs, goals and descendants remains separate work, with offline acknowledgements and independent local tasks preserved.
