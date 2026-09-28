# 共享历史读取的撤权竞态 / Revocation during Edge history reads

状态：已完成。确定性认证 WebSocket 测试、Server Clippy、真实账号共享与长历史浏览器回归全部通过。

Node 历史分页、完整事件及实时补读在发送 RPC 前核验会话权限；等待 Node 回复期间撤销共享时，引用投影会过滤不可见的其他会话，但原始目标会话的数据仍可能返回给旧读者。新增在事件投影完成、释放响应前复核当前目标会话的查看权限，覆盖在线、缓存及运行结果中的事件。

测试使用真实认证 WebSocket：先授权查看并发起读取，收到 Node 命令后撤销共享，再发送私有历史回复。三个读取入口均必须拒绝原读者；已保存的 canonical 事件仍可由所有者读取，拒绝响应不包含私有正文。不使用睡眠推测竞态时序。

Node history and event reads checked access before RPC, allowing a share to be revoked while awaiting the response. Reference projection filtered other sessions but did not reauthorize the original target. The fix rechecks target view access after event projection and before returning online, cached or run-result events. An authenticated WebSocket test revokes the share after receiving the read command and only then sends the private result. All three RPC read paths must deny the former reader while preserving the owner's canonical cache; no sleeps establish ordering.

浏览器回归保留四类共享撤权读取拒绝、成员流停止和所有者继续运行断言；30,005 条事件的 Local／Server 分页、实时续接和离线缓存也通过。

Validation passed: the deterministic authenticated transport test, Server Clippy, real sharing/revocation browser workflow and the 30,005-event Local/Server pagination, Live continuation and offline-cache browser regression.
