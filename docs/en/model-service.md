# Platform and account model service

Server offers a model service independently of where a task executes. Administrators publish/grant platform models; users manage private account providers in **My models**; connected clients and Node sessions consume only their authorized selections.

## Publish a model

In **Platform administration → Model service**, configure an upstream provider, protocol, endpoint and secret. Define its model catalog, context/output limits and reasoning mappings, then publish the intended models and grant access to users or model-service groups. Model-service groups and team collaboration permission groups serve different purposes.

Provider discovery does not expose the stored secret to the browser. Editing a key uses the secret replacement behavior on the provider card. Choose a protocol matching the upstream rather than relying on a model name or hostname. See [models](models.md).

## Authorize clients

A standalone client can connect to Server's model service through explicit browser authorization. The user chooses the model source and allowed grant/device scope. Private account models are not automatically added to an existing platform authorization. Revoking a device or grant ends subsequent access without copying its key to another source.

A remotely controlled Node session selects an account/platform model through Server's session authorization. It does not need a separate platform API key placed on the Node. Shared resources use the owner's chosen binding; collaboration rights do not enumerate unrelated private providers or substitute the submitter's private key.

## Metering and revocation

Server reserves and settles platform usage against the selected grant and records attempts. Retries recheck authorization and limits; unavailable or revoked sources do not silently fall back. A cancelled or uncertain upstream response must not be represented as a known zero-cost result. See [unknown usage reconciliation](model-usage-reconciliation.md).

Direct computer-provider observations are separate from Server-metered requests and are not double charged. See [device-provider usage](device-provider-usage.md). Model API compatibility is the documented protocol contract, not a claim that every upstream vendor feature is supported.

A member with configuration permission can delegate their own platform grant or account Provider to a hosted session. Existing delegated sources retain their model owner; unrelated private catalogs are not exposed. Model-owner configuration permission is rechecked during calls and retries, and other submitters require platform-grant resource sharing. Resource ownership, execution reservations, model ownership and the actual submitter remain distinct. The model source remains tied to the original run/submitter permissions across schedules and subagents. WorkerPolicy controls execution limits, not upstream secrets or the publication catalog. See [Server reference](server-reference.md).

Claude Provider owners can explicitly enable hosted web search/fetch. The gateway adds the configured tool definitions after authorization and preserves pause responses and citations. Client-submitted tool definitions remain limited to functions. See [hosted web tools](web-access.md#claude-hosted-web-tools).
