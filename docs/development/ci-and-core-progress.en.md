# CI and client/Server progress

English · [简体中文](ci-and-core-progress.md)

Status: all CI checks and four-platform packages for `76801de` passed. The [Checks run for `b177e44`](https://github.com/mosttt/ternilo/actions/runs/36463338780) is now validating the merged recovery and history changes. Native account recovery and bounded history are merged into main after local validation; unknown-usage reconciliation is also merged after database/browser validation; direct Provider usage visibility is next.

## Scope and findings

Establish Apache-2.0 licensing, verify CI from a clean GitHub checkout, and prioritize the local client and Server workflow. Work remains a separate design reservation.

The repository initially had no commits and its GitHub remote was empty. The pinned Linorun revision is available remotely and matches the clean local checkout. Local Web, CLI and Node share `LocalApplication`; Server routes workspaces to Node or the optional Worker. Accounts, OIDC, enrollment, streaming, sharing, model access and database recovery already have implementations and tests.

Existing workflows referenced missing LICENSE, THIRD_PARTY_NOTICES.md and licenses files, blocking desktop compilation and packaging. These files have been supplied, with Apache-2.0 declarations aligned across Rust, SDKs, Web, desktop packages and container images. Third-party notices retain their original licenses.

CI now includes LocalApplication, builtins, authorization, RPC/ACP, real Python/TypeScript SDK calls, browser tests for groups and model service, and PostgreSQL Node routing under restricted runtime grants.

## Verification

Initial commit `da0d9bf` was pushed to main and started the [first Checks run](https://github.com/mosttt/ternilo/actions/runs/36443218372). Dependency checks passed remotely. Linux exposed a directory-picker test that asserted focus restoration before Radix's unmount timer ran; it now waits for the actual focus condition while preserving the assertion, and its six tests pass locally.

The [Checks run for `12c1e85`](https://github.com/mosttt/ternilo/actions/runs/36446651439) passed dependency checks, Windows and both macOS targets, but Linux failed two search tests because `rg` was unavailable; packaging was skipped. Linux CI now explicitly installs and checks ripgrep, the Debian desktop package declares it, and installation documentation covers the external runtime requirement. The first two runs were superseded by fixes. There are no version tags or GitHub Releases yet; checks and packaging must complete first.

Continue with CI and downloadable artifacts, then the P1/P2 work below, keeping the client and Server first and Work as a design reservation.

The [Checks run for ripgrep fix `76801de`](https://github.com/mosttt/ternilo/actions/runs/36451490404) passed Linux core/SDK/PostgreSQL/browser/native-desktop/recovery checks, Windows, both macOS targets and dependency gates. Platform packages are building. The [implementation plan](remaining-core-plan.md) records subsequent requirements. Recovery commit `6d00781` and history commit `72b3530` are merged into main. Recovery covers credential/session issuance races and browser reauthentication. History covers Local, Node, Server, archives and Live continuity; see the [recovery](native-account-recovery.md) and [history](bounded-session-history.md) records. Downloaded Linux CI artifacts passed SHA256 and license checks; their actual binaries passed complete-directory, SQLite-snapshot and PostgreSQL restore browser workflows.

Local checks passed: 148 Web test files / 932 unit tests, seven rich-text tests, TypeScript, i18n, documentation links, production Web build, 30 deployment/release script tests, operations configuration, actionlint, workspace Clippy, dependency license/source/advisory checks, npm audit, and both SDK smoke tests.

The first local browser run used older binaries. Asset hashes and OIDC contracts showed they did not match current sources, so those results are excluded from current-version acceptance. Matching binaries have since been rebuilt. Rust tests and doctests for the client, Server, Local, Builtins, Authorization, Automation, ACP, Control, Protocol, Kernel, Transport, Transport Store, Storage and plugin CLI all passed. A workflow file or a successful compilation alone does not establish release acceptance.

After rebuilding matching binaries, SDK file operations, five local/account/group/model-service browser scenarios, six OIDC/security/PWA/mobile scenarios, and two delivery plus SQLite/PostgreSQL recovery scenarios passed. Three additional PostgreSQL tests passed for authentication settings, OIDC sessions and Node routing under restricted runtime grants. Responsive group/model tests now wait for the application's viewport height to synchronize after resize before retaining all geometry and touch-target assertions. The second GitHub run passed Web/deployment/dependency checks and macOS Apple Silicon compilation; the latest commit's complete matrix is still being verified.

CI also caches dependency build artifacts, using a pinned cache Action and the repository's Rust toolchain, with target and Linorun revision separation. Failed jobs retain dependency caches for the next run. Workspace code still undergoes normal Cargo compilation checks.

The final local build included the client, Server, plugin CLI and desktop with `tauri/custom-protocol`. Both native desktop tests passed, covering cold/hot deep links, single-instance forwarding and reuse of a CLI service. The latest GitHub run passed dependency checks and macOS Apple Silicon compilation and saved build caches. Linux, Windows, macOS Intel and subsequent packaging still depend on that run's final results. A subsequent documentation-only commit records this evidence without repeating the complete build; code verification refers to `12c1e85`.

## Remaining work, in order

1. **P0: CI and delivery acceptance.** Complete clean GitHub checks and platform packaging. Signing, installation/uninstallation and native runtime checks need their own evidence.
2. **P1: Native account recovery.** Authentication-settings reset and account unbanning do not reset a forgotten password. Define a local operator recovery path before email-based self-service recovery. Preserve identity and resources; invalidate old sessions. Evidence: `apps/ternilo-server/src/admin.rs`, `platform/identity.rs`, and [security](../security.md).
3. **P1: Large history reads.** Live supports deltas, but historical and archived sessions still load complete event snapshots. Add bounded pagination across storage, Node transport, Server and Web, including reconnect and archive preview. Evidence: `crates/ternilo-local/src/application/history.rs` and [product limits](../product.md).
4. **P1: Complete usage visibility.** Device-direct Provider usage is not aggregated at Server, and unknown usage lacks a complete reconciliation workflow. Preserve deduplication, attribution, late settlement and reservations. Evidence: gateway tests and [model service](../model-service.md).
5. **P2: Shared-resource lifecycle.** Project-wide inheritance, ownership transfer and complete task cleanup after account deactivation remain missing. Define effects on running work, revoked shares and files. Evidence: account-status tests and [product limits](../product.md).
6. **P2: Remote automation and scaling.** SDKs currently spawn local stdio processes. Server HTTP/WebSocket SDKs and reconnect support remain absent; multiple Server instances require routing to connection owners and synchronized notifications. Evidence: [automation](../automation.md), `platform/edge/connection.rs`, and [deployment](../deployment.md).

These are verified gaps, not delivery promises. Prioritize one Server managing local computers and VPS nodes. Work and multi-Server scaling are not prerequisites.

## Work reservation

Follow the [Work execution design](execution-coordination.en.md): separate persistent Work IDs from container IDs, reuse versioned executor contracts and workspace routing, preserve authorization and usage attribution, and manage containers from the host. No Work service, database table, Docker scheduler or placeholder UI has been added.
