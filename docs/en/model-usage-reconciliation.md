# Reconcile unknown model usage

English · [简体中文](../zh-CN/model-usage-reconciliation.md)

When an upstream omits complete usage or a call disconnects, Server retains a reservation for unknown consumption. Reserved tokens are not actual usage, and missing counts are not zero.

An Owner or Admin can open **Model service → Usage**, expand a request's attempts, and select **Reconcile usage** for a terminal attempt that reached the upstream but lacks complete usage. Verify its upstream record first, then enter input/output tokens, optional cache/reasoning details, an evidence reference and a note. Input/output are required; enter `0` only when the verified count is zero. Blank optional counters remain unknown. Reference an upstream record or internal reconciliation ticket; do not enter credentials.

The request list, usage totals and quotas then reflect the completed counters. The original actor, beneficiary, grant, accounting month, outcome and error remain unchanged. Cache/reasoning details do not add another charge. Managed attempts also refresh their original execution reservation and budget period. Calls can be reconciled after their key or grant is revoked.

**Reconciliation records** retain the administrator, timestamp, evidence, explanation and previous partial usage in the immutable platform audit, committed atomically with settlement. Operators and Auditors may read records but cannot reconcile; ordinary accounts cannot use these management endpoints. Identical retries return the original record without another charge or audit entry. Conflicting corrections and late complete upstream usage share the request lock; the first complete settlement cannot be overwritten.

Active requests, unattempted calls and already known usage cannot be corrected here. Device-reported direct Provider usage is outside this authoritative ledger. This is an audited administrative operation; it does not query the upstream or independently prove the counters.

## Management API

- `GET /api/v1/admin/models/requests/{request_id}/reconciliations` requires model-service read permission.
- `POST /api/v1/admin/models/requests/{request_id}/attempts/{attempt}/reconcile` requires grant-management permission.

Send `expected_settled_at_ms` from the current attempt, a `usage` object containing input/output and optional cache/reasoning counters, plus `reference` and `note`. [The Chinese guide includes a complete JSON example](../zh-CN/model-usage-reconciliation.md#管理接口). Changed attempts, known usage and conflicting corrections return 409. Caller-provided `raw_usage` is rejected. Existing upstream raw usage is retained. The operation uses the existing database schema and requires no migration.
