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

A shared session uses its owner's selected private model only within the allowed sharing scope. Configuration permission does not reveal unrelated private providers or keys. Scheduled work preserves its original input author and rechecks permissions. Direct device-provider usage is observation data, distinct from Server-metered model settlement; see [device usage](device-provider-usage.md) and [unknown usage reconciliation](model-usage-reconciliation.md).
