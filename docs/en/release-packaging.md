# Build and package releases locally

Use published assets for installation. Building from source requires the pinned Rust toolchain, Node.js and the matching sibling Linorun checkout. See [contributing](contributing.md). GitHub builds the supported platform matrix described in [CI and releases](ci-release.md).

## Component archives

From the source checkout:

```bash
scripts/package-release.sh --component local --version 0.1.2 --output-dir /path/to/releases
```

`local` builds the standalone client and plugin utility, `server` builds Server, and `worker` builds the optional Worker. Omitting the component selects `all`. Archive prefixes are respectively `ternilo-`, `ternilo-server-`, `ternilo-worker-` and `ternilo-all-`. Windows uses ZIP when `--binary-suffix .exe` is supplied; Linux/macOS use tar.gz to retain executable permissions.

For already verified binaries:

```bash
scripts/package-release.sh --component local --version 0.1.2 --no-build --bin-dir /path/to/verified-binaries --target-name linux-x86_64 --output-dir /path/to/releases
```

`--target-name` names an artifact; it does not cross-compile or guarantee compatibility. Existing outputs are not overwritten. A `.sha256` sidecar accompanies each archive, and `RELEASE` records its component and program list. Windows local packages require the matching `ternilo-sandbox-windows.exe` helper. See [program roles](binaries.md).

Packages include bilingual entry pages, `docs/zh-CN/`, `docs/en/`, deployment templates, referenced examples, SDK sources, profiles and licenses. They exclude development records, live environment files, secrets, databases and backups. Shared documentation does not mean every component executable is present. Preserve `LICENSE`, `THIRD_PARTY_NOTICES.md` and `licenses/` when redistributing.

An extracted binary package is not a complete Cargo/Web source tree. Repository build and test commands require the source checkout. Linux local execution needs `bubblewrap`, and file search needs `ripgrep`; these are not bundled into portable client archives or AppImage. The Debian installer declares dependencies. Desktop installers are built separately through Tauri.

## Images

Production [Compose](docker-compose.md) pulls `ghcr.io/mosttt/ternilo-server:0.1.2` without building locally. For custom images, explicitly select your own image name and add `compose.server.build.yml` to the Compose files. Worker uses its own build override and image. Both require matching source inputs; an extracted portable package is not the build context.

An offline deployment can transfer images using `docker image save` and `docker image load`, then configure the exact tag or digest. Container license files are under `/usr/share/ternilo/`; desktop license files are in application resources. Custom build concurrency and dependency download concurrency are operator choices, not machine-specific repository defaults.

## Acceptance

The optional full Docker gate is `deploy/docker/release-gate.sh`; it accepts explicit Server, Worker and acceptance-test image names plus an artifact directory. It tests the actual production images and records logs, exact image IDs and checksums. Passing tests of the gate script is not equivalent to running the full gate.

Client/Server installation recovery has an independent entry point:

```bash
TERNILO_E2E_NODE_BINARY=/path/to/verified-binaries/ternilo \
TERNILO_E2E_SERVER_BINARY=/path/to/verified-binaries/ternilo-server \
TERNILO_E2E_PLUGIN_BINARY=/path/to/verified-binaries/ternilo-plugin \
TERNILO_E2E_ARTIFACT_DIR=/path/to/evidence \
node --test web/tests/delivery-restore-acceptance.test.mjs
```

Use binaries embedding the same Web build. The test packages into isolated temporary directories, validates binary bytes/checksums, exercises bundled SDKs, model configuration and Node registration, and restores SQLite snapshots and stopped full archives. It uses temporary model fixtures, not production keys. Debug binaries validate the installation flow only; final release binaries still require their own acceptance. This test is not a schema migration tool or proof of Worker/physical-device recovery.
