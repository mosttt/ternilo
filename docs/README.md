# Ternilo Documentation

[English](README.md) · [简体中文](README.zh-CN.md)

Start with the [local quickstart](getting-started.md). To manage computers from another device, continue with [remote access](remote-access.md). Complete a real task first, then configure collaboration, plugins, or managed execution as needed.

The entry pages and account recovery guide are available in English and Simplified Chinese. Other detailed guides linked below are currently written in Simplified Chinese.

## Installation and deployment

- [Local quickstart](getting-started.md): start Ternilo, open a folder, configure a model, and submit a task.
- [Remote access](remote-access.md): deploy a Server, connect computers or VPS instances, and work from other devices.
- [Server operations](deployment.md): initialization, databases, reverse proxies, diagnostics, backups, and recovery.
- [Desktop application](desktop.md): native windows, background services, notifications, and deep links.
- [Managed Worker](worker.md): configure and maintain the optional execution service; the separate Work container model is not yet implemented.

## Everyday use

- [Workspaces and conversations](user-guide.md): projects, folders, sessions, queues, permissions, and exports.
- [Model configuration](models.md): providers, protocols, reasoning, context windows, and request timeouts.
- [Settings and presets](settings.md): configuration ownership, Agent presets, tool calls, and execution limits.
- [Messages and files](files.md): attachments, context references, generated files, previews, and downloads.
- [Tools and multiple agents](agents.md): skills, plans, workflows, subagents, and background services.
- [Accounts and collaboration](collaboration.md): computers, independent drafts, sharing permissions, and submitter identity.
- [Web access troubleshooting](web-access.md): fetching, search, proxies, and DNS.

## Administration

- [Platform administration](platform-management.md): access modes, registration, accounts, teams, and managed execution.
- [Authentication and human verification](server-authentication.md): OAuth 2.0/OIDC, Turnstile, secrets, and login recovery.
- [Native account recovery](account-recovery.en.md): reset an existing password through the local operator CLI while retaining identity and resources.
- [Platform model service](model-service.md): published models, grants, access keys, client authorization, and usage.
- [Capabilities and limitations](product.md): deployment modes, resource sharing, supported features, and boundaries.
- [Security model](security.md): identity, credentials, approvals, operating-system isolation, and telemetry.

## Development and maintenance

- [Architecture](architecture.md): program responsibilities, data ownership, and execution paths.
- [Server reference](server-reference.md): routing, transactions, metering, and HTTP interfaces.
- [Extension configuration](extensions.md) and [extension packages](extension-packages.md): plugins, MCP, LSP, hooks, Rhai, WASM, and signing.
- [Automation and SDKs](automation.md): CLI, JSON-RPC, ACP, Python, and TypeScript.
- [Development and validation](contributing.md): source setup, scoped tests, browser checks, and database verification.
- [Build and package](release-packaging.md): component archives, versioned images, and offline delivery.
- [CI and GitHub releases](ci-release.md): checks, platform binaries, desktop installers, GHCR images, and release drafts.

Documentation is organized around usage and maintenance tasks. Unimplemented technical designs live in `docs/development/` and are not included in delivery packages.

Commands assume the repository or extracted package root unless stated otherwise. Replace example domains, paths, accounts, and secrets with your own values; they do not identify public services.

- [Reconcile unknown model usage](model-usage-reconciliation.en.md): complete missing counters with administrative evidence and immutable audit.
