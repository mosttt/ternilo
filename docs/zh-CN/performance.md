# 并发读取性能验证

[English](../en/performance.md)

`web/tests/server-read-load.mjs` 创建独立 Server、已登记 Node、工作区和合成长会话，在电脑在线和离线两种状态下运行闭环并发请求。覆盖已认证的工作台状态、最近历史、较早历史以及整月设备用量；每个响应同时检查内容，不能用错误页或空数据提高吞吐量。

本机／Node 的最近历史页直接读取日志尾部，较早页先按序号定位对应字节位置，再读取该页；不需要额外磁盘索引。缓存切换、分页数量和 Live 续读各自负责不同的读取开销。

先构建匹配的程序，再执行测试：

```bash
npm --prefix web ci
npm --prefix web run build
cargo build --locked --release -p ternilo -p ternilo-server
TERNILO_E2E_NODE_BINARY="$PWD/target/release/ternilo" \
TERNILO_E2E_SERVER_BINARY="$PWD/target/release/ternilo-server" \
node web/tests/server-read-load.mjs \
  --build-label release --concurrency 1,8,32 --seconds 10 \
  --sessions 4 --deltas 10000 --attempts 200 \
  --output ./read-load-result.json
```

默认生成 4 个会话，每个包含 10000 条流式思考片段和 200 次合成 Provider 尝试，共 41612 个原始事件。没有真实模型请求、费用或用户资料。所有进程只监听回环地址，临时目录和进程在结束后清理；仅保留指定 JSON 结果。每个并发用户等待当前请求结束后再发下一个请求，四类操作轮换，不是开放式固定到达率负载。

输出记录源码提交、工作树是否有改动、两个程序的 SHA-256／版本、构建标签、操作系统／CPU／内存、测试规模及每个阶段的吞吐量、成功数量、响应字节数、p50／p95／p99／最大耗时和错误。耗时包含 HTTP、响应读取与客户端 JSON 校验。测试前每类操作先预热；结束时等待已开始的请求完成，吞吐量按实际阶段耗时计算。任何请求错误、超时或内容不一致都会使测试失败并保留结果。

PostgreSQL 使用独立的空数据库，名称必须以 `ternilo_load_` 开头。通过受保护的环境配置提供 `TERNILO_LOAD_DATABASE_URL`（受限运行身份）和 `TERNILO_LOAD_MIGRATION_DATABASE_URL`（建表身份），其余命令相同；原始数据库不自动删除。不要指向运行实例或备份数据库。默认不设置这两个变量，使用临时 SQLite。

比较时使用相同程序构建模式、数据规模、并发设置和主机资源；不要同时编译或运行其他压测。开发构建的延迟、一次短时运行、同机回环流量都不能推导生产容量承诺。此负载没有覆盖模型流式并发、写入饱和、真实上游限流、多 Server 故障转移或长时间稳定性；这些需要独立场景。开发验收与本阶段优化记录见[读取压测记录](../development/read-load-performance.md)。

## 模型请求、流式取消与持续压力

`web/tests/server-model-load.mjs` 启动独立 Server 和回环合成上游，通过真实模型 API 产生准入、请求／尝试账本和结算写入。四组独立 Provider／授权／密钥共用同一提交账号。测试普通 JSON、SSE、并发超限、客户端在收到首段数据后取消，以及持续负载；不调用真实收费模型。

构建匹配程序后执行：

```bash
TERNILO_E2E_SERVER_BINARY="$PWD/target/release/ternilo-server" \
node web/tests/server-model-load.mjs \
  --build-label release --concurrency 1,8,32 --seconds 10 \
  --soak-seconds 300 --latency-ms 25 --output ./model-load-result.json
```

`--instances 2` 使用两个独立 Server 进程共享同一个受限 PostgreSQL 数据库；仍须通过前述 `TERNILO_LOAD_DATABASE_URL`／`TERNILO_LOAD_MIGRATION_DATABASE_URL` 提供独立空库，库名以 `ternilo_load_` 开头。SQLite 只支持本脚本的单实例场景。所有密钥、模型目录和临时配置由脚本生成，结束清理进程与目录；原始 PostgreSQL 库和指定 JSON 结果保留。

每个响应检查完整内容，预期的 429 必须包含 `rate_limited` 与 `Retry-After`。报告分别计算已接受吞吐量、预期限流响应率、JSON／流式／取消耗时，不能把 429 当成模型成功吞吐量。取消不推断为没有费用：最终按每月分页读取完整账本，核对已接受 ID 恰好出现一次、上游实际调用数一致、活动租约清零、已知 Token 精确结算和未知尝试保留预留。账本按原 UTC 月份归组，当前月份另与 Server 汇总核对，不把当前月份汇总当成整场数据。遇到意外错误停止增加请求，保留失败阶段；不会靠重试掩盖错误。

默认持续阶段 60 秒，可设置 0–3600 秒。持续时长、客户端并发、上游延迟和构建标签均记录；这仍是闭环同机合成负载，不代表真实 Provider 延迟、开放式到达率容量或生产 SLA。跨 Server 的 Node 指令转发与存储故障恢复不在这个模型 API 场景内，须独立验收。
