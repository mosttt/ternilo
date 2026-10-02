# Node routing across Servers

[简体中文](../zh-CN/server-cluster.md)

The current source includes cross-Server Node forwarding, live-change catch-up and computer-model streaming, verified in a real two-instance browser flow with a restricted PostgreSQL runtime role. Deploy matching versions containing these features. Sharing PostgreSQL and routing calls do not establish complete storage failover.

Participating Servers must use the same version, database and `secret_master_key`. Each sets its own `cluster_url`, reachable by other instances. This identifies one instance rather than a load balancer, and is separate from the browser login `public_url`. Every instance needs a distinct address that routes directly to it.

```json
{
  "cluster_url": "https://server-a.internal.example:8443"
}
```

Add the field to the full private configuration, or use `ternilo-server serve --cluster-url https://server-a.internal.example:8443` or `TERNILO_SERVER_CLUSTER_URL`. Other instances advertise their own origins. Cross-host traffic requires HTTPS with certificate verification. A reverse proxy forwards `/api/v1/internal/node/call` to the selected instance without logging request bodies or authentication signatures. HTTP accepts only literal loopback addresses such as `127.0.0.1` and `::1` for processes on one host.

The Node keeps one authenticated WebSocket. A request arriving at another Server finds the active connection lease, contacts its holder over the authenticated online channel, and returns the original result. The receiver rechecks the lease, original Node credential and executor capabilities. Inputs retain their actual author and account authorization; returned content retains resource permission checks. Forwarding grants no additional computer or resource access.

Keep instance clocks synchronized: request authentication accepts at most 30 seconds of clock skew. Inputs retain their admission authority revision, so disabling and re-enabling an account cannot refresh an old input into newly authorized work.

Transient credentials, directories and file contents do not enter the shared command journal. A connection failure does not prove that execution never occurred. Without a verified result, forwarding does not automatically resend the operation. Inspect the conversation, queue and actual files before deciding what to do next.

Each Node has a coalesced change revision in the shared database. Other instances catch up authorized history and current state while browsers keep their normal live subscriptions. Rolled-back transactions produce no committed hint. Synchronized history remains available offline; operations requiring the computer return unavailable after disconnect or lease expiry.

This coordinates Node connections and calls without moving directories, relocating Server/Worker storage, taking over unsupervised external processes or implementing Work container scheduling. Existing quotas, restricted PostgreSQL, backup and persistent-volume requirements remain in the [deployment guide](deployment.md). See the [architecture](architecture.md#remote-paths) for scope.

Cross-computer models use the separate `/api/v1/internal/node/model` WebSocket channel. Reverse proxies must also allow WebSocket upgrades on that path. The channel verifies its signature, freshness, source owner, current connection lease and executor capability. Output, attempt reports, cancellation and fresh retry permits travel over the same connection. Requests do not enter durable replay queues; replacing the source connection ends in-flight calls.
