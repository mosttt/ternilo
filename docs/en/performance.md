# Concurrent read performance validation

[简体中文](../zh-CN/performance.md)

`web/tests/server-read-load.mjs` creates an isolated Server, enrolled Node, workspace and synthetic long conversations. It exercises authenticated workbench state, recent/older history and monthly device usage while the computer is connected and offline. Every response is validated; errors or empty results cannot inflate throughput.

Build matching programs and run:

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

Defaults create four conversations, each with 10000 streaming reasoning fragments and 200 synthetic Provider attempts: 41612 canonical events in total. No real models, charges or user data are involved. Processes listen on loopback, and temporary files/processes are cleaned up. Only the chosen JSON report remains. Each virtual user waits for a response before issuing its next request, rotating through four operations; this is a closed-loop workload, not a fixed arrival rate.

Reports include source commit, dirty-tree status, binary SHA-256/version, build label, OS/CPU/memory, workload settings and per-phase throughput, successful counts, response bytes, p50/p95/p99/maximum latency and errors. Timings include HTTP transfer and client JSON validation. Each operation warms up before a phase. In-flight requests finish before the phase ends; throughput uses actual elapsed time. Any HTTP error, timeout or invalid content fails the run and preserves evidence.

For PostgreSQL, provide an empty disposable database whose name begins with `ternilo_load_`. Set `TERNILO_LOAD_DATABASE_URL` for its restricted runtime identity and `TERNILO_LOAD_MIGRATION_DATABASE_URL` for its schema owner through protected environment configuration. The database is not automatically deleted. Never target a running instance or a backup database. Without these variables the test uses temporary SQLite.

Compare the same build profile, data scale, concurrency and hardware without simultaneous compilation or other load tests. Development builds, short trials and loopback traffic do not establish production capacity. Model streaming concurrency, write saturation, real upstream throttling, multi-Server failover and long-running stability require separate workloads. See the Chinese [development results](../development/read-load-performance.md) for this stage's measurements.
