# Ternilo documentation

[English](README.md) · [简体中文](../zh-CN/README.md)

Choose an entry point below, complete a first task, then read the features you need. Command examples use installed executable names on `PATH`.

## Start here

| Your situation | Reading order |
|---|---|
| Work on this computer | [Local quickstart](getting-started.md) → [User guide](user-guide.md) |
| Use a desktop window | [Desktop](desktop.md) → [Local quickstart](getting-started.md) |
| Continue from a phone or another computer | [Deploy Server](deployment.md) / [Docker Compose](docker-compose.md) → [Connect computers](remote-access.md) |
| Collaborate with other accounts | [Remote access](remote-access.md) → [Collaboration](collaboration.md) → [Platform management](platform-management.md) |
| Provide managed execution | [Worker deployment](worker.md) |

`ternilo` is the client and `ternilo-server` the shared remote entry point. Personal computers and VPS instances use the same client connection procedure. Worker is optional managed execution; Work remains a separate planned component.

## Use the workbench

- [User guide](user-guide.md): workspaces, sessions, queues, task permissions and history.
- [Model configuration](models.md) and [platform model access](model-service.md): your own services or explicitly granted models.
- [Settings and presets](settings.md): language, theme, configuration targets and Agent presets.
- [Files and attachments](files.md): references, uploads, previews and downloads.
- [Tools and agents](agents.md): skills, goals, plans, schedules, background work and workflows.
- [Web diagnostics](web-access.md): fetching, search and proxy connections.

## Manage Server and collaboration

- [Server deployment](deployment.md) and [Docker Compose](docker-compose.md): protected web setup, SQLite/PostgreSQL, HTTPS, operations and backups.
- [Computers](computers.md): immutable IDs, editable names, details, pause/resume, revocation and removal.
- [Platform management](platform-management.md), [login configuration](server-authentication.md) and [account security](account-recovery.md): access modes, accounts, OIDC, verification, passwords and MFA.
- [Collaboration](collaboration.md), [project sharing](project-sharing.md) and [management handoff](resource-management.md): membership, resource rights, attribution and ownership.
- [Service accounts](service-accounts.md): automation identities, credentials and explicit grants.
- [Device model usage](device-provider-usage.md), [reconciliation](model-usage-reconciliation.md) and [performance](performance.md): budgets, ledgers, exports, limits and validation scope.
- [Multiple Servers](server-cluster.md): shared database, instance origins, Node routing and event catch-up.
- [Configuration and data layout](data-layout.md): settings, persistent data, credentials and backups.

## Extend and develop

- [Automation and SDKs](automation.md) and [remote SDKs](remote-sdks.md): CLI, JSON-RPC, ACP, Python and TypeScript.
- [Extension configuration](extensions.md) and [extension packages](extension-packages.md): MCP, LSP, Hooks, Rhai, WASM, signing and installation.
- [Development](contributing.md): checkout, environment, builds and focused validation.
- [Architecture](architecture.md) and [Server reference](server-reference.md): modules, storage, transactions and interfaces.
- [Package programs](binaries.md), [packaging](release-packaging.md) and [CI/releases](ci-release.md): executables, installers, images and publication checks.
- [Identifiers and credentials](identifiers.md) and [session log repair](session-log-repair.md): current names and offline maintenance.

See [capabilities and boundaries](product.md) for supported behavior and [security](security.md) for permissions and isolation. Formal guides have Chinese and English editions; `docs/development/` holds Chinese development records. Planned work in those records is not a delivered feature.
