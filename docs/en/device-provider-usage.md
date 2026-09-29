# Device Provider usage

[简体中文](../zh-CN/device-provider-usage.md)

In Server, open My Models → Usage and select Device local, a space, one of your computers, and a UTC month. Search by Provider/model and page through synchronized attempts. Summary counters cover only the current page, not the entire month.

A task can make multiple model calls, retries and auxiliary calls such as title generation. Each actual attempt is recorded separately. Input, output, cache read/write and reasoning counters retain their protocol meanings. Missing fields remain unreported; explicitly reported zero stays zero. A start without a finish remains incomplete and does not establish that no upstream consumption occurred. Submitters come from the original task: local user, automation, or a Server-verified account. Unverified account claims are not presented as trusted identities.

These are device reports. Server-proxied requests retain their separate authoritative ledger and quotas. Official connected-Server models and managed gateways do not also create direct-device observations. All Server sources covers only the Server ledger. A manually configured Provider pointing at Server may produce records in both separate views, without charging device reports to platform budgets.

Computers must be enrolled for remote control with matching executor protocol `44`. Model-only client authorization does not upload sessions or grant computer access. While Server is offline, observations remain in the computer journal and synchronize after reconnection. Replaying an event does not create another attempt; copied fork history does not count as new calls. Older calls without observations and unsynchronized data are outside coverage. An empty list does not imply no calls occurred.

Only the computer owner can read its report. Session sharing or a space administrator role does not grant computer-wide usage access. Reports contain public session references, Provider/model/protocol, attempt status and upstream request IDs, without prompts, credentials, raw responses or upstream error bodies. They depend on retained original journals and Server session mappings. Deleting those records removes their observations; fork copies do not become a replacement ledger. This is an operational view, not an immutable bill or proof of upstream charges.

See the [Server reference](../zh-CN/server-reference.md) for API pagination and permissions. [Administrative reconciliation](model-usage-reconciliation.md) handles unknown counters in the Server ledger and does not change device reports.
