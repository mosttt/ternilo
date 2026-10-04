<p align="center">
  <img src="https://raw.githubusercontent.com/mosttt/ternilo/main/web/public/assets/icon.svg" alt="Ternilo logo" width="88" height="88">
</p>

<h1 align="center">Ternilo</h1>

<p align="center"><strong>Put ideas in motion.</strong><br>An AI assistant for your files and tools, on this computer or across connected computers.</p>

<p align="center">
  <a href="https://github.com/mosttt/ternilo/actions/workflows/ci.yml"><img src="https://github.com/mosttt/ternilo/actions/workflows/ci.yml/badge.svg?branch=main" alt="CI status"></a>
  <a href="https://github.com/mosttt/ternilo/releases/latest"><img src="https://img.shields.io/github/v/release/mosttt/ternilo?color=456bdc" alt="Latest release"></a>
  <a href="https://github.com/mosttt/ternilo/pkgs/container/ternilo-server"><img src="https://img.shields.io/badge/GHCR-ternilo--server-456bdc?logo=docker&amp;logoColor=white" alt="Server container image"></a>
  <a href="LICENSE"><img src="https://img.shields.io/github/license/mosttt/ternilo?color=456bdc" alt="Apache-2.0 license"></a>
</p>

<p align="center">
  <a href="docs/en/getting-started.md">Quickstart</a> ·
  <a href="docs/en/README.md">Documentation</a> ·
  <a href="docs/en/deployment.md">Deployment</a> ·
  <a href="https://github.com/mosttt/ternilo/releases">Downloads</a> ·
  <a href="https://github.com/mosttt/ternilo/issues">Feedback</a>
</p>

<p align="center"><a href="README.md">English</a> · <a href="README.zh-CN.md">简体中文</a></p>

---

Ternilo is an AI assistant that reads project files, runs tools and keeps the history of its work. Use it on your computer, or connect computers and VPS instances to a Server for remote access and collaboration.

## Start on your computer

Download a [desktop installer or client archive](https://github.com/mosttt/ternilo/releases) for your operating system. Open the desktop app, or install the CLI executables on `PATH` and run:

```sh
ternilo serve --open-browser
```

Choose a working folder, add your model connection under **Settings → Models**, and send a task. Ternilo shows the response and tool activity as work progresses. The [local quickstart](docs/en/getting-started.md) covers installation, the first model connection and stopping the service.

Local use needs only `ternilo`. The desktop app and browser connect to the same local service. Interface language follows your browser's Chinese/English preference until you save a manual choice.

## Use your computers remotely

Run `ternilo-server` on a computer or server reachable by your other devices. Use the [Docker Compose guide](docs/en/docker-compose.md), or follow [native Server deployment](docs/en/deployment.md).

Server starts with a protected web setup page: copy the Initialization Key from its logs, choose SQLite or PostgreSQL, and create the administrator. Once configured, restarts use the saved database and account.

Sign in to Server and open **User settings → My computers**. Generate a connection command and run it on the computer that will execute tasks. Open Server from your phone or another browser, choose that computer and continue working. Files and tools remain on the execution computer; it connects outbound, so its local port does not need public exposure.

For teams, the owner enables multi-user mode and grants access to specific resources. Model access and workspace access are separate permissions. See [remote access](docs/en/remote-access.md) and [collaboration](docs/en/collaboration.md).

## Choose a guide

| Your task | Guide |
|---|---|
| Start locally and complete a first task | [Local quickstart](docs/en/getting-started.md) |
| Install or use the desktop app | [Desktop](docs/en/desktop.md) |
| Deploy Server with Docker, SQLite or PostgreSQL | [Docker Compose](docs/en/docker-compose.md) |
| Run Server binaries, configure HTTPS or back up data | [Server deployment and operations](docs/en/deployment.md) |
| Connect computers and VPS instances | [Remote access](docs/en/remote-access.md) |
| Configure models, files, tools and sessions | [User guide](docs/en/user-guide.md), [models](docs/en/models.md) |
| Manage accounts, computers and shared resources | [Platform management](docs/en/platform-management.md), [computers](docs/en/computers.md) |

The [documentation index](docs/en/README.md) includes SDKs, extensions and operations. Managed execution uses an optional [Worker](docs/en/worker.md). The planned Work container component remains a separate design.

## Capabilities

Ternilo provides workspaces and sessions, streaming responses, file and command tools, persistent terminals, background tasks, attachments, history search and export. It supports OpenAI Chat and Responses, DeepSeek Responses, native Gemini and Claude Messages, plus models authorized by Server.

Presets combine plugins and tools, including skills, plans, explicitly enabled goals, scheduled tasks, subagents, MCP, LSP and signed Rhai/WASM extensions. Task permissions control file access and execution. Server adds accounts, computer management, resource sharing, model grants, usage and audit.

See [capabilities and boundaries](docs/en/product.md) for supported behavior, isolation limits and remaining work.

## Development

Source builds require the Rust toolchain in `rust-toolchain.toml`, Node.js and the Linorun revision pinned in `.github/linorun-revision`. Follow the [development guide](docs/en/contributing.md) for checkout, build and validation commands, and [packaging](docs/en/release-packaging.md) for distributable binaries.

`apps/` contains programs, `crates/` shared Rust modules, `web/` the interface and `sdk/` client libraries. Formal documentation is in `docs/en/` and `docs/zh-CN/`; `docs/development/` contains Chinese development records.

[CI and releases](docs/en/ci-release.md) validate the code, native platforms and deployment, then publish client/Server archives, desktop installers and the Server image. Available downloads are listed in [Releases](https://github.com/mosttt/ternilo/releases).

## License

[Apache-2.0](LICENSE). Third-party components retain their own licenses; see [third-party notices](THIRD_PARTY_NOTICES.md).
