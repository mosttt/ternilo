# CI and GitHub releases

The workflows separate checking, packaging and publishing. Ternilo is checked out beside Linorun; `.github/linorun-revision` pins the matching Linorun commit. Rust, npm dependencies and external Actions are pinned in the repository.

## Workflows and artifacts

**Checks** runs on pull requests, main pushes and manual dispatch, and is reused by Release. It covers Web, documentation, Rust, deployment scripts, dependencies, real browsers, native Linux desktop checks and SQLite/PostgreSQL behavior. Windows and both macOS architectures run compile and persistence checks. Successful main/manual checks call packaging; pull requests do not publish binaries.

**Build packages** creates optimized binaries, desktop installers and checksums for Actions artifacts. Running it manually produces a candidate, not a GitHub Release. **Release** verifies the version, then runs Checks, platform packaging and Server image validation in parallel. Image publication waits for all checks and builds; assets become public only for a matching version tag. Dispatching Release against a branch validates candidates without publishing.

| Platform | Rust target | CLI and Server | Desktop |
|---|---|---|---|
| Linux x86_64 | `x86_64-unknown-linux-gnu` | `.tar.gz` | `.deb`, `.AppImage` |
| Windows x86_64 | `x86_64-pc-windows-msvc` | `.zip` | `.exe`, `.msi` |
| macOS Apple Silicon | `aarch64-apple-darwin` | `.tar.gz` | `.dmg` |
| macOS Intel | `x86_64-apple-darwin` | `.tar.gz` | `.dmg` |

Client archives use `ternilo-<version>-<target>`. Server archives use `ternilo-server-<version>-<target>`. Windows client packages include the sandbox helper. Formal documents are under `docs/zh-CN/` and `docs/en/`; development records, live secrets and runtime data are excluded. See [package contents](binaries.md).

Linux binaries are built on Ubuntu 24.04; earlier glibc systems are not guaranteed. The Server image is built separately on Debian Bookworm. ARM64 Linux and Windows are outside this matrix. Desktop installers are currently unsigned/unnotarized, and automatic updates are disabled. A successful build is not evidence of installation on every physical target.

## Publication

Keep workspace and Tauri versions identical, update docs, then push a matching `v<version>` tag from the intended commit. Release uploads all assets before making the release public. Verify both public asset access and anonymous image access after completion. A GitHub source archive alone is not a binary release.

The image is `ghcr.io/<owner>/ternilo-server:<version>`, currently `linux/amd64`. A `sha-<source-commit>` tag also identifies it; no drifting `latest` tag is required. `server-image.txt` records the registry digest. The build job validates initialization, native login, embedded Web, non-root execution, a read-only root filesystem and identity preservation after restart, then retains the tested image as an Actions artifact for one day. Once all checks and platform packages pass, the publishing job loads that image and verifies its image ID against the build job before pushing it. Expired candidates require rebuilding. Work and Worker images are outside this publishing workflow.

Only the image publishing job receives `packages: write`; only the Release publishing job receives `contents: write`. Candidate builds have read-only repository permissions. Pull requests use read-only permissions and temporary test services. Registry package visibility must be checked separately from repository visibility.

Checks cancel obsolete work per workflow/ref/event/job. Platform packaging finishes its active build and retains the newest queued candidate. Release runs serialize per tag. Caches accelerate dependencies but never replace verification. See [contributing](contributing.md) and [local packaging](release-packaging.md).
