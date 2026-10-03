# 并发读取性能验证

[English](../en/performance.md)

`web/tests/server-read-load.mjs` 创建独立 Server、已登记 Node、工作区和合成长会话，在电脑在线和离线两种状态下运行闭环并发请求。覆盖已认证的工作台状态、最近历史、较早历史以及整月设备用量；每个响应同时检查内容，不能用错误页或空数据提高吞吐量。

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
