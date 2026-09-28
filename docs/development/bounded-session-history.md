# 有界会话历史 / Bounded session history

状态：实现和本机完整验证通过，已加入 CI。 / Status: implementation and local end-to-end validation passed; included in CI.

Local、Node、Server 和网页共用 `before_seq` 排他游标，默认 200 条、最多 1000 条；返回升序事件和下一页游标。活跃 Session 从日志内存切片，关闭的 Session 从 JSONL 尾部按块读取，数据库按 seq 倒序限量查询后恢复升序。Node 页面沿用路径脱敏、权限、来源投影和引用转换；离线读取已有连续缓存，不将尾页冒充完整复制记录。

Local, Node, Server and Web share an exclusive `before_seq` cursor, with 200 events by default and a maximum of 1000. Active sessions use an in-memory slice; closed sessions read JSONL blocks backwards; database queries use descending sequence limits and return ascending events. Node reads retain access checks, path redaction, provenance projection and reference translation. Offline pages contain only replicated history.

网页首次读取尾页，再从最后原始 seq 接续 Live，旧页合并不改动实时游标。请求取消与 generation 检查覆盖会话／账号切换；翻页失败保留游标以便重试。对话、轨迹和归档预览均提供较早记录读取。现有完整事件、导出和 Live 接口保持语义；新增 Node 操作将执行器协议从 42 升为 43。

Web loads the latest page before subscribing to Live at the last canonical sequence. Prepending history never rewinds Live. Cancellation and generation checks discard stale session/account responses; failed pages retain their cursor for retry. Chat, trajectory and archive views expose earlier records. Full event reads, exports and the Live wire protocol retain their semantics; the new Node operation bumps the executor protocol from 42 to 43.

边界：首次看到的单轮推理可能从中段开始，可继续补读。离线不能返回尚未同步到 Server 的记录。JSONL 较早页需从文件末尾跳过较新记录；Cloud 派生统计仍回放完整事件，本次未将其冒充增量投影。

Limits: a page can start inside a turn and earlier reasoning can be loaded on demand. Offline history excludes unsynchronized records. Older JSONL pages scan newer records from the file tail; Cloud derived statistics still replay the full journal.


已通过：148 个 Web 测试文件／935 项测试；最后滚动锚点调整后 50 项受影响测试、TypeScript、i18n 与文档检查；客户端／Server 及相关库 Clippy；本地 JSONL 分块读取和活跃／归档生命周期；SQLite 与受限 PostgreSQL 的 Edge 历史契约；Cloud PostgreSQL 归档分页与权限契约。最终匹配构建的 4 项浏览器测试全部通过，覆盖三万条事件、本地／Server 实时续接及离线分页、长归档桌面／移动端、只读共享、撤销、恢复和中英文切换。Protocol／Transport 的 57 项测试通过。

Passed: 148 Web test files / 935 tests; 50 affected tests after the final scroll-anchor adjustment, TypeScript, i18n and documentation checks; application and library Clippy; local JSONL block reads and active/archive lifecycle; SQLite and restricted PostgreSQL Edge contracts; Cloud PostgreSQL archive pagination and access contracts. All four browser scenarios passed against the final matched build: 30,005 events, Local/Server Live continuation, offline pages, long desktop/mobile archives, read-only sharing, revocation, restoration and language switching. All 57 Protocol/Transport tests passed.
