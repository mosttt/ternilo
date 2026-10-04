# Concurrent read performance validation

[简体中文](../zh-CN/performance.md)

`web/tests/server-read-load.mjs` creates an isolated Server, enrolled Node, workspace and synthetic long conversations. It exercises authenticated workbench state, recent/older history and monthly device usage while the computer is connected and offline. Every response is validated; errors or empty results cannot inflate throughput.

Local/Node history reads the latest page from the journal tail and locates older pages by sequence and byte offset, without an additional disk index. Cached switching, page sizes and Live continuation address separate reading costs.

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

## Model admission, stream cancellation and sustained load

`web/tests/server-model-load.mjs` starts isolated Server processes and a loopback synthetic upstream, exercising the real model API, admission and request/attempt settlement writes. Four independent Provider/grant/key routes share one submitting account. Phases cover JSON, SSE, expected concurrency limits, client cancellation after the first chunk and sustained load. No paid model is called.

After building matching binaries, run:

```bash
TERNILO_E2E_SERVER_BINARY="$PWD/target/release/ternilo-server" \
node web/tests/server-model-load.mjs \
  --build-label release --concurrency 1,8,32 --seconds 10 \
  --soak-seconds 300 --latency-ms 25 --output ./model-load-result.json
```

`--instances 2` uses independent Server processes sharing restricted PostgreSQL. Supply the disposable empty database through `TERNILO_LOAD_DATABASE_URL` and `TERNILO_LOAD_MIGRATION_DATABASE_URL`; its name must begin with `ternilo_load_`. SQLite runs one instance. Generated credentials, model configuration and temporary directories belong to the fixture; processes/directories are cleaned up, while PostgreSQL and the requested JSON evidence remain.

Responses are validated, including `rate_limited` and `Retry-After` for expected 429s. Admitted throughput and rejection rate are reported separately, alongside JSON/stream/cancellation timings. The final ledger is read page by page and checked for exactly one row per accepted ID, matching upstream call counts, no active leases, exact known token settlement and retained reservations for unknown cancelled usage. Ledger rows retain their original UTC month; the current month is also reconciled against Server's aggregate, which is not treated as the whole-run total. Unexpected errors stop new requests and preserve failed evidence without retrying them away.

The sustained phase defaults to 60 seconds and accepts 0–3600. The report records duration, concurrency, upstream delay and build label. This is closed-loop, same-host synthetic traffic rather than real Provider latency, open-loop capacity or a production SLA. Cross-Server Node RPC and storage failure recovery need separate scenarios.
