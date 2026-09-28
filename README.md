# Ternilo

[English](README.md) · [简体中文](README.zh-CN.md)

Put ideas in motion.

Ternilo is an AI assistant that works with project files, runs commands, and preserves the history of its work. Use it on your own computer, or connect multiple computers and VPS instances through a Server for remote access, collaboration, and shared model access.

## Choose your setup

| What you need | What to run | Start here |
|---|---|---|
| Work on this computer | `ternilo` | [Local quickstart](docs/getting-started.md) |
| Access your computers from a phone or another device | `ternilo` + `ternilo-server` | [Remote access](docs/remote-access.md) |
| Collaborate and share model access | A Server in multi-user mode with resource permissions | [Deployment](docs/deployment.md), [platform administration](docs/platform-management.md) |
| Provide managed execution | A Server with managed execution enabled and an optional `ternilo-worker` | [Worker deployment](docs/worker.md) |

Both single-user and multi-user modes support personal computers and cloud VPS instances, with SQLite or PostgreSQL. A VPS can run the ordinary `ternilo` client; a Worker is optional. The planned Work container model is separate from the existing Worker.

## Get started

After extracting a package built for your operating system, run:

```bash
./bin/ternilo serve
```

Open the address printed in the terminal, choose a workspace folder, and add a model connection under **Settings → Models**. Then describe your task. If no usable model is configured, the interface guides you through setup without discarding your draft.

The local web interface listens only on loopback. For remote access, your computer connects outbound to a Server, where you sign in. The desktop application connects to the same local service; closing a browser or desktop window does not stop running tasks.

Available binary packages and images are listed in the repository's release records. You can also [build component packages](docs/release-packaging.md) yourself. Running a package does not require Rust, Node.js, or Linorun source code.

[GitHub CI and releases](docs/ci-release.md) cover checks, client and Server binaries, desktop installers, and the Server container image. Main-branch builds produce candidate artifacts; version tags run the release checks, publish the Server image, and create a draft release.

### Run from source

Ternilo depends on a matching sibling checkout of [Linorun](https://github.com/mosttt/linorun). The required commit is pinned in `.github/linorun-revision`. Preserve any local changes before switching an existing Linorun checkout.

```text
/path/to/source/
├── linorun/
└── ternilo/
```

Install the Rust toolchain specified in `rust-toolchain.toml` and Node.js, then run these commands in the Ternilo repository:

```bash
npm --prefix web ci
npm --prefix web run build
cargo run --locked -p ternilo -- serve
```

Linux also requires `bubblewrap` for confined commands. See the [development guide](docs/contributing.md) for platform requirements and validation commands.

## Features

- Workspaces and sessions, streaming conversations, collapsible reasoning and tool activity, history search, and export.
- OpenAI Chat, OpenAI Responses, DeepSeek Responses, native Gemini, and Claude Messages; use your own models or access granted by a Server.
- File operations, commands, persistent terminals, background tasks, attachment previews, and generated-file downloads.
- Skills, plans, goals, scheduled tasks, subagents, Agent Teams, and workflows.
- Configurable presets and plugins, MCP, LSP, web tools, Rhai, and signed Rhai/WASM extensions.
- Read-only, workspace-write, and full-access task permissions, with approval where required.
- Server-managed accounts, computers, teams, resource sharing, model grants, usage, and auditing.
- A shared foundation for web, desktop, PWA, CLI, JSON-RPC, ACP, and Python/TypeScript SDKs.

See [product capabilities and limitations](docs/product.md) for supported behavior and boundaries. Validate capacity against your own hardware and workloads before deployment.

## Documentation and source

The [documentation index](docs/README.md) organizes installation, everyday use, administration, and development. Entry pages are available in English and Simplified Chinese; detailed guides are currently in Simplified Chinese.

`apps/` contains program entry points, `crates/` shared Rust modules, `web/` the workbench, and `sdk/` client libraries. `deploy/` and `scripts/` provide deployment and packaging tools. `docs/development/` contains technical designs that are not yet implemented.

## License

Ternilo is licensed under [Apache-2.0](LICENSE). Third-party components retain their own licenses; see [third-party notices](THIRD_PARTY_NOTICES.md).
