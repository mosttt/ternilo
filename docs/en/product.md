# Product capabilities and boundaries

[简体中文](../zh-CN/product.md)

Ternilo is for people who want an AI agent to work with real files and tools. It supports personal computers, VPS instances, multiple users and model authorization. Managed execution is optional. Transparent cross-Server failover and measured production capacity are not yet promised.

## Individual and team use

Single-user versus multi-user mode concerns who can access and collaborate, not whether a computer is at home or in a cloud. Both modes support multiple computers, SQLite or PostgreSQL, and optionally platform models or managed Workers. Multi-user mode adds independent accounts, personal spaces, teams and resource sharing.

The instance owner changes mode in the Web interface. Switching modes does not replace the database, delete accounts or move files. A person can run `ternilo` locally without Server or a team.

## Resources

| Resource | Responsibility |
|---|---|
| Account | Username, stable system ID and contact email; receives a personal space |
| Personal/team space | Membership, organization and management boundary |
| Project | Groups workspaces within a space; does not create or synchronize directories |
| Workspace | Binds an execution computer and working directory, or managed storage |
| Session | Conversation, tasks, events and configuration within a workspace |
| Working directory | Files the agent operates on; distinct from private application data |

A project can include workspaces on a laptop and a VPS. New sessions and forks do not copy project files. Local/Node directory admission uses trusted input authors: one user may run concurrently, while different users' overlapping directories exclude each other. Managed Workers retain execution-family ownership and exit proofs. Concurrent access does not solve simultaneous file-edit conflicts. Organization membership never implies model budget access.

## Separate sharing dimensions

Model sharing authorizes an account, device or constrained key to call a model with specified limits. Workspace/session sharing grants use of an execution environment and its permitted files and tools. Project/work sharing lets people view, submit, stop, configure or fork existing work according to permissions.

These rights compose but do not substitute for one another. Team membership is not model credit; execution access is not a reusable model key; a session grant is not access to every project resource. Project inheritance is explicitly enabled by each workspace manager. [Management handoff](resource-management.md) is supported within a team and retains storage/execution identity. Real-browser and restricted PostgreSQL handoff acceptance passed. File and execution-identity migration is not provided.

Platform model groups batch model grants. Team permission groups batch resource permissions. Resource owner, submitting actor and billing source remain distinct, including shared tasks.

## Interface and programs

The workbench is for choosing a working location, submitting tasks and reading results. User Settings contains personal preferences, connections and configuration. My Models contains granted model access. Team Management handles members and collaborative rights. Platform Administration handles accounts, upstream model supply, policies, Workers and audit. An administrator does not automatically receive private resources or model secrets.

Shared sessions keep independent drafts for each user and feed accepted inputs into the session queue. User messages retain real authors. Live events synchronize output; directory coordination does not replace task division between parent and child agents.

- `ternilo` executes on the user's computer and stores local configuration, sessions and attachments. Its local management interface is loopback-only. Node is its role after connecting to Server.
- `ternilo-server` provides the remote entry point, identity, resource permissions, computer routing, model gateway and usage ledger. Model service can operate without a Worker.
- `ternilo-worker` optionally runs managed tasks in isolation. It holds neither the Server database connection, master key nor upstream model keys.

Desktop connects to the local service. A module boundary does not imply a separate mandatory process. Work is a distinct future container-management design, not another name for Worker.

## Current limits

| Area | Available | Remaining boundary |
|---|---|---|
| Multiple computers | Local and connected computers, remote folders, file tasks and cached history | Offline history contains only synchronized events; Cloud projections still replay events |
| Models | Account/device/platform sources, remote-computer model forwarding, collaborator-provided managed models, independent client grants, actor-preserving child tasks and schedules | More provider-hosted tools and independent model authorization policies |
| Protocols | OpenAI Chat, OpenAI Responses, DeepSeek Responses, Gemini and Claude Messages; Claude hosted search/fetch | Other provider-hosted tools, Vertex AI and Bedrock |
| Accounts | Native accounts, invitation/open/approval registration, bans/removal, OIDC, unified local sign-in session management, Turnstile, self-service passwords, verified email recovery, TOTP/recovery codes, operator recovery and space-scoped service account identity/credentials, workspace grants and Live/SDK access | Passkeys are not available; service accounts use explicitly authorized session models, with separate model-owner checks |
| Collaboration | Teams, groups, direct grants, manager-enabled project inheritance, team management handoff, attribution and queue conflicts | File/execution-identity migration and more granular model delegation |
| Usage | Per-attempt ledger, budgets, keys, device limits, late settlement, reconciliation, monthly device-report CSV platform/account rate and concurrency limits and monthly cross-computer CSV | Additional account-wide monthly budgets |
| Execution | Local directory coordination, background services and basic same-host Worker recovery | Work pools, dedicated workspace storage and cross-host handoff |
| Extensions | Signed Rhai/WASM, capabilities, resource ceilings and running-tool cancellation | Synchronous host I/O and Hook cancellation follow their own lifecycles |
| Scale | One Server with SQLite/PostgreSQL; transactional permission notifications; Node forwarding, event catch-up and model streaming verified through a restricted PostgreSQL two-instance browser flow | Storage failover and capacity claims |

Continuous goals support immediate execution, additional rounds, blocked completion, stopping and explicit continuation. Durable history does not authorize replay of effects after a crash. Archives support read-only preview and restoration without automatically starting work. Offline log repair is available. MCP supports tool-list refresh and optional bounded reconnection without replaying failed calls; more search providers and external-agent adapters remain separate work.

PWA caching makes the interface shell available offline; it cannot execute remote work while disconnected. Linux, macOS and Windows have separate isolation implementations, with incomplete read isolation on Windows. Compilation does not establish native installer, signature or physical-machine acceptance. See [security](security.md), [automation](automation.md), [collaboration](collaboration.md) and [deployment](deployment.md).
