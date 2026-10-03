# Model configuration

Models have three explicit sources: providers stored on the execution computer, private providers belonging to a Server account, and models published/granted by Server's platform service. A model with the same name in another source is not an automatic fallback. Select the intended source, provider, model and reasoning level in the conversation.

## Configure a provider

Open local **Settings → Models**, or Server's **My models** and select the account/computer target. Add a provider ID, display name, base URL and protocol. Enter the API key on the same card. Saved secrets are not returned to the browser; leaving the key field empty while editing preserves the existing key, and entering a new value replaces it.

Add models manually or fetch the upstream catalog. Discovery runs through the execution/server endpoint and does not disclose the saved key. Configure context window, maximum output tokens, reasoning capabilities, timeouts and retry policy to match the actual upstream.

Supported protocol families include OpenAI Chat Completions, OpenAI Responses, DeepSeek Responses, Gemini and Anthropic. Choose the actual protocol; the application does not silently infer a different protocol from a hostname. DeepSeek Responses retains the upstream reasoning content needed for follow-up tool calls. Standard OpenAI Responses uses its reasoning-summary behavior. Missing reasoning text is never fabricated from token usage.

## Catalog and reasoning

For each capability, explicit manual values override upstream-provided values, which override provider defaults. Context capacity, output capacity and reasoning configuration are independent. Catalog refresh can update upstream data while preserving your manual overrides and names. Search filters discovery results without discarding selections hidden by the filter; changes persist only when saved.

Reasoning choices use the provider/model's declared mapping. A removed or unavailable level is shown honestly; reselect an available level to update the conversation. Existing accepted work keeps its captured model parameters. Revoked grants cannot be replaced by a same-named device or account model.

## Account and platform use

Connected standalone clients can receive explicit Server authorization for account or platform models. Remote-control Node sessions instead use their Server-side session authorization; they do not require copying platform API keys onto the Node. See [model service](model-service.md).

A member with configuration permission can delegate their own account Provider or platform grant to a managed session. The picker separates the account's own catalog from the session's existing delegated source; it does not enumerate someone else's unrelated private providers or grants. Account selections include the model owner's identity. Keeping an existing source or adjusting its reasoning level preserves that owner. Scheduled work preserves its original input author and rechecks permissions. Direct device-provider usage is observation data, distinct from Server-metered model settlement; see [device usage](device-provider-usage.md) and [unknown usage reconciliation](model-usage-reconciliation.md).

## Use another computer's model in a remote session

In a Server-managed session on execution computer A, open Model → Models on other computers. Choose one of your computers B in the same space, then its Provider/model. The directory fetches B's catalog only after you select B. Names are displayed; bindings retain immutable computer IDs across renaming and reconnection.

A runs the agent, files and tools. Model requests travel A → Server → B; B calls its upstream with its own configuration and key, then streams results back through Server to A. Keys are not copied to A or used to call the upstream directly from Server. This selection is exclusive to remote computer sessions; it cannot become a standalone-client or managed-workspace default.

Initially selecting B requires ownership of B and configuration permission on the session. Authorized collaborators and service accounts may submit work using the saved binding while the source owner retains configuration permission. Requests, retries and active execution recheck authorization. An offline or paused source, revoked credentials, or changed Provider/key stops availability or execution. The system does not fall back to a same-named local Provider or replay interrupted requests. Computers connected to different Server instances use the [cluster channel](server-cluster.md).

My Models → Usage → Device local → Cross-computer model calls loads separate request records on demand. They distinguish execution/source computers, the submitter and model owner. Counters are source-computer reports, separate from platform budgets; missing counters stay unreported. See [device usage](device-provider-usage.md).

## Claude's official model catalog

For Anthropic's official API, choose `anthropic-messages` and set the base URL to `https://api.anthropic.com/v1`. Discovery uses `GET /v1/models`, `x-api-key` and `anthropic-version: 2023-06-01`, rather than Chat Completions Bearer authentication. The base URL is the version root, not a full `/messages` or `/models` endpoint.

Discovery follows `has_more`, `last_id` and `after_id`, preserving `display_name`, `max_input_tokens`, `max_tokens` and explicitly returned capabilities. Missing reasoning levels retain explicit configuration instead of inventing support. Reference: [Anthropic Models API](https://platform.claude.com/docs/en/api/models/list).

Discovery failures show the upstream HTTP status with guidance about keys, permissions, the versioned API root or rate limits. Connection failures, timeouts and non-JSON catalogs have separate messages. Raw upstream error bodies and keys are not returned to the browser.

Managed execution, storage and task reservations stay with the original resource owner. The model provider and actual submitter may be different accounts; explicitly authorized service accounts can also submit. Model calls, retries and active execution recheck the model owner's configuration permission. Platform grants must allow resource sharing for other submitters. Removing the model owner's configuration permission stops model calls even if the submitter retains workspace access. Worker receives no upstream key.
