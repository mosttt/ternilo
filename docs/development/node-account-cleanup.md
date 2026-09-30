# Node 账号撤销与执行端收尾

状态：实施前只读审查完成，尚未实现。当前 Server 禁止旧输入补发，托管任务已有[账号清理](account-task-cleanup.md)，Node 完整清理不能据此宣称完成。

## 必须保留的执行边界

Node 断连后已接收的命令可能继续运行。普通 `CancelRun` 回执表示已发出取消请求，不证明后台进程退出；普通持久命令会到期，`ReplyCache` 的重启后未知状态也不能作为清理完成回执。账号重新解封不应恢复旧授权输入。

`LocalApplication::recover_session_work` 会在 Server 握手之前恢复使用本机 Provider 的持久 schedule。仅保护模型网关或连接后的 `resume_server_schedules` 不够：恢复授权屏障需要在打开应用之前安装，并持久保存 Server 绑定。Account 输入及其自动派生任务先等待权威同步；省略 gateway 参数不能把它们变成本机输入。独立 Local 输入继续可用。

取消必须按可信执行归属选择目标，不能用电脑所有者、最新消息、用户名或 Automation 类别代替账号。Node 排队编辑目前只改内容并保留 provenance，尚未传递编辑者的执行授权；若采用托管运行的编辑转授权语义，需要独立可信元数据。

## 三个实施切片

1. **持久请求与启动屏障。** 在账号状态事务内写独立 outbox，以账号、撤销版本、Node 与凭据实例定位；复用 Gateway 输入接收作者与版本证据，覆盖本人电脑及接收过该账号输入的他人电脑。正常认证继续拒绝撤销凭据，另设用途受限的 `CleanupPrincipal`，只能领取与该凭据实例绑定的清理及提交收尾回执，不能进入正常连接池、恢复电脑 active 状态或访问 Application、文件、模型与上传接口。Node 先持久接收，再执行可恢复的清理；第一阶段保持 pending。
2. **归属与选择性收尾。** 保存执行账号、接收版本、根输入与实际 run 映射。复用 `InputOrigins::resolve` 的 schedule 创建事件与子会话回溯；人工子任务跟进属于新的独立授权。禁止目标旧输入 claim，终结其排队项，停止所属 run、持续目标、schedule 和派生资源，保留其他账号及 Local 输入。Jobs 已有 `JobId → RunId`；Terminals 当前丢弃 `ToolExecutionContext`，需补创建归属。Subagent 的 interrupt／dispose 提前写 Cancelled，仍需等待实际 driver 退出。
3. **完成回执与界面。** 只有目标驱动退出、所属后台资源收尾、目录租约释放、恢复项永久禁用之后，才持久保存完成回执。Server 验证凭据实例和撤销版本后显示 confirmed；离线、已投递及已请求取消保持 pending，失败保留原因。复用 Edge Live 通知刷新管理员清理状态。重复请求与 ACK 丢失后重传应幂等；重新登记后的电脑不接受旧实例 ACK。

三个切片合起来完成后才宣称 Node 完整闭环。验收使用混合 Alice／Bob／Local 队列、人工子任务跟进、schedule、持续目标、后台 job 和 terminal，以及真实外部写入进程；覆盖 Server／Node 重启、离线撤销、快速封禁解封、收尾中断、重复请求和旧凭据普通接口拒绝。界面需实际检查 pending → confirmed、控制台及网络请求。

## 不能省略的进程证明

现有 Unix 进程组终止仅发信号，Windows 的普通 `process_group` 实现为空。Rust task 退出不能自动证明各平台进程树退出，完成 ACK 要连接实际进程监督通知。重启后无法证明旧外部进程已经收尾时，保留待确认或失败，不能伪造完成。
