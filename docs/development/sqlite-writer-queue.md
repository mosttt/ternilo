# SQLite 写事务排队与持续模型压力

状态：SQLite 写锁故障已复现并修正，修正已发行；存储合同、业务回归和 SQLite 同规模持续压测通过。2026-10-09 使用公开 v0.2.8 发布构建的受限 PostgreSQL 双实例持续复测通过；此前 SIGKILL 的发送者仍未确定，不把本次通过当作其根因已修复。

2026-10-03 在实际 Server、合成回环上游与独立 SQLite 上运行模型负载。32 个并发客户端持续执行模型请求时，先出现数秒级长尾，随后 HTTP 500。第二次独立空库复现，Server 明确记录 `server database error: (code: 5) database is locked`，不是上游模型错误。修正前证据在 [sqlite-model-before.json](benchmarks/2026-10-03/sqlite-model-before.json)：持续阶段 176.73 秒内验证 3668 个正常响应后因一次 500 停止新增请求；不能将这次失败写成 300 秒通过。

原实现允许连接池中的多个连接同时执行 `BEGIN IMMEDIATE`；它们在 SQLite 内等待唯一写锁，并占住可供读取的连接。新实现为文件型 SQLite 保留一个独立写连接，其余用于读取。请求先进入 SQLx 的异步池等待队列，`Database` 的 Clone 在 Control／Cloud／Gateway 之间共享同一写队列。总连接数仍为原 `max_connections`；单连接和内存库沿用单池，PostgreSQL 保持原池与数据库并发策略。

不包裹或改写事务语义：`Transaction` 仍是 SQLx 事务，提交／回滚／取消释放连接。新存储合同在持有未提交写事务时排入 16 个写入者，验证读取仍可获得已提交快照；取消队列中请求不写入，取消已取得连接的事务回滚，下一写入者能正常继续。已有 WAL 快照、共享事务与 schema 合同同时通过。

修正后的验收使用相同 32 并发、5 秒阶段、300 秒持续阶段和 25 毫秒合成上游。存储与受限 PostgreSQL 权限、业务模块及实际浏览器流程结果如下。此结果是开发构建的本机诊断，不是生产容量保证，也不能据此宣称跨 Server 的那次独立 503 已修复。

存储 SQLite 4 项和 PostgreSQL 2 项合同通过；Control 54 项、Cloud 28 项单元回归，以及 8 项受限 PostgreSQL 控制合同通过。独立身份 PostgreSQL 合同首次运行缺少其专用测试库环境变量，补齐单独空库后，17 个身份／撤权／锁顺序场景定向重跑通过。真实 Chromium 的 MFA、跨电脑模型任务／月度 CSV 和分层限流 3 个完整流程通过。新程序的模型负载短跑完成逐个请求 ID、上游次数、已知／未知结算和汇总核对，同规模复测结果见下。

并发压测驱动另补 `--instances 2 --rpc`：通过一个 Server 接收浏览器侧读取、另一个 Server 连接 Node，在线阶段追加命令目录和事件读取，专门覆盖此前未重现 503 的内部转发路径；离线阶段仍只验证缓存读取。2026-10-04 使用独立受限 PostgreSQL、2 个 Server、2 个会话／每会话 1043 个合成事件，在线／离线 1、8、16 并发各 5 秒，完成 1217 次正确响应，无错误。在线通过跨实例命令和完整事件读取；这份短场景没有复现此前的 503，也不能将其根因记录为已确定，见 [postgres-rpc-reads.json](benchmarks/2026-10-03/postgres-rpc-reads.json)。

2026-10-04：[sqlite-model-after.json](benchmarks/2026-10-03/sqlite-model-after.json) 完成 301.56 秒持续阶段，5550 次正常响应；包括限流、取消阶段在内的 6087 次实际上游调用逐个核对请求账本，6067 条已知、20 条未知用量，无活跃请求或重复结算，无 HTTP 500／写锁错误。持续阶段 JSON／流式响应 p95 约 3.04 秒，仍有进一步性能优化空间；不把这份开发构建诊断当成容量保证。

PostgreSQL 双实例复测多次出现一个 Server 被 SIGKILL、另一个仍存活；保留的 Server 日志没有 panic 或数据库错误。一次采样中两实例 RSS 最高约 84 MB，所在 cgroup 未记录 OOM；这些证据不能确定发送信号的来源。此阶段记录为中断，不能将已通过的双库合同／双 Server 功能浏览器验收扩大为长时间压力通过。

2026-10-04：增加仅用于本机诊断的 Node 子进程信号跟踪后，双实例复测的准入／限流／取消阶段通过，持续阶段在 91.06 秒、2219 次正确响应后因一个 Server 收到 SIGKILL 中断。原始报告见 [postgres-model-signal.json](benchmarks/2026-10-04/postgres-model-signal.json)，信号顺序见 [process-signal-trace.jsonl](benchmarks/2026-10-04/process-signal-trace.jsonl)。Server 1651071 在 04:38:56.366 UTC 退出，跟踪中此前没有 Node 的 child.kill／process.kill 调用；另一个实例 1651109 的 SIGINT 清理在 04:38:56.654 才发生。所在 cgroup OOM 计数与全局 /proc/vmstat oom_kill 均为 0；来源仍不能确定，不再盲目重跑相同压力场景。

另修正公共测试辅助 stopProcess：在发送停止信号前订阅退出事件，正常退出时清除强制停止定时器，超时发送 SIGKILL 后也等待实际退出。避免已退出进程的多余信号及测试进程等待未清理定时器。修改后两个实际本机／Server 长历史浏览器流程再次通过，包含多次停止／启动和 Node 离线读取。诊断用信号跟踪没有固化为项目 CI 或标准部署。

2026-10-09：公开 v0.2.8 Server 在独立 PostgreSQL 容器中使用无建表权限的 runtime，另用建表连接初始化。两个实际 Server、1／8／32 并发短阶段、限流、取消及 32 并发 300 秒持续阶段全部正常退出。持续阶段实际 301.07 秒、12703 次正常响应，无 HTTP 错误；JSON／流式 p95 分别约 2759／814 毫秒。全部阶段 14199 次实际调用逐项匹配请求账本，14123 条已知用量、76 条未知用量，实际 token 593166、保留预留 395884，活跃请求为零，无重复结算。报告见 [postgres-model-release.json](benchmarks/2026-10-09/postgres-model-release.json)。

临时 Node 信号观察只有正常创建及 SIGINT 收尾，没有 SIGKILL；cgroup OOM 计数前后均为零，见 [process-signal-trace.jsonl](benchmarks/2026-10-09/process-signal-trace.jsonl) 与 [pressure-observation.json](benchmarks/2026-10-09/pressure-observation.json)。当前主机没有可用的内核信号跟踪入口，Node 观察只能证明本测试发出的信号，不能识别过去的外部发送者。本次使用已公开程序并保留其摘要和实际来源提交；驱动仓库 HEAD 另行记录，不能混为二进制来源。负载端、两个 Server 和数据库共享同一台主机，测试期间有独立浏览器验收，本次属于诊断复测，不是生产容量或最大吞吐承诺。
