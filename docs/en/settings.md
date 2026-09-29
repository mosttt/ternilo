# Settings, presets and execution limits

The theme defaults to **System** and responds to operating-system appearance changes while the page is open. An explicit Light or Dark selection remains your preference. Settings also control language, conversation text size, completed-turn display and the default behavior of future conversations.

## Configuration ownership

Check the target shown at the top of settings. Computer-scoped providers, credentials, plugins, presets and attachments are stored on that computer. Server routes changes to it; an offline computer cannot accept those changes. Account providers and private API keys belong to your Server account. Cloud presets belong to the current user in the selected space, and cloud extension inventory belongs to the space.

Project files stay in the selected computer directory or managed Worker storage. Server stores accounts, sharing and hosted metadata. Closing a browser does not delete data or stop tasks. Back up each relevant location; a Server backup cannot restore files on a personal computer.

Ordinary model keys are configured on provider cards. **Credentials and sign-in** is for independent plugin credentials and authorization flows. See [getting started](getting-started.md) and [models](models.md).

## Browser sessions

Server's **User settings → General → Account** lists this account's unexpired native browser sessions. It shows the current session, browser/OS description, first and latest source IP, sign-in time, last activity and expiry. Expand browser details to see the supplied User-Agent and public session ID. Login tokens and token hashes are never included.

A normal browser does not reveal the computer's hostname. The interface states this rather than substituting the Server hostname. Browser/OS descriptions come from User-Agent and are descriptive, not verified device identity. IPs describe the address visible to Server, which can be a VPN, NAT or proxy address. HTTP activity is normally persisted once per minute; a changed source IP is recorded immediately. Passive reading or an idle WebSocket does not imply new HTTP activity. Older sessions show missing metadata honestly until activity is observed.

Only explicitly configured proxy addresses may supply `X-Forwarded-For`. Configure exact addresses with `--trusted-proxy-ips` / `TERNILO_SERVER_TRUSTED_PROXY_IPS`, or `trusted_proxy_ips` in Server JSON. Untrusted forwarding headers are ignored. These addresses identify your reverse proxies, not all potential clients.

You can revoke one session or all other sessions. Revoking the current session asks for confirmation and signs out. With OIDC, the list still manages native sessions issued by this Server; external identity-provider sessions remain under that provider's control. See [authentication](server-authentication.md).

## Agent presets

Built-in `standard`, `ptc`, `minimal` and `creative` presets are read-only starting points. Copy one, edit its fields and plugins, save, then optionally make it the default for new conversations. PTC retains standard capabilities but exposes them to the model through Rhai Code Mode. Minimal focuses on files and shell; creative adds guidance for runtime experiments.

A conversation captures a preset snapshot. Editing or deleting the original does not rewrite existing conversations. Only a blank conversation that has never accepted work can change its preset. Cancellation, removal from the queue, restart or a history-preserving fork does not make a used conversation blank again. Start a new conversation for another preset. Models and execution permissions have their own controls.

## Limits

The Agent defaults to 512 tool calls per turn; zero removes that Agent-level ceiling. Each `wait_agent` call counts once regardless of waiting duration. Steps count model decisions, not tokens or individual tool calls; the default step limit is zero.

Edit `agent-loop` in the current conversation's plugin configuration while idle, or edit a custom preset for future conversations. A host can impose a stricter ceiling using `--max-tool-calls` and `--max-steps`. Positive limits combine by taking the smaller value; zero means no extra limit at that layer. Changing launch options requires restarting the service. Server/Worker host limits come from WorkerPolicy; accepted managed jobs retain their captured configuration. Manual stop remains available independently of limits.
