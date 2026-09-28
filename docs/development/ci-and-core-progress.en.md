# CI and client/Server progress

English · [简体中文](ci-and-core-progress.md)

Status: in progress. This records implementation work and remaining gaps; [product documentation](../product.md) defines supported behavior.

## Scope and findings

Establish Apache-2.0 licensing, verify CI from a clean GitHub checkout, and prioritize the local client and Server workflow. Work remains a separate design reservation.

The repository initially had no commits and its GitHub remote was empty. The pinned Linorun revision is available remotely and matches the clean local checkout. Local Web, CLI and Node share `LocalApplication`; Server routes workspaces to Node or the optional Worker. Accounts, OIDC, enrollment, streaming, sharing, model access and database recovery already have implementations and tests.

Existing workflows referenced missing LICENSE, THIRD_PARTY_NOTICES.md and licenses files, blocking desktop compilation and packaging. These files have been supplied, with Apache-2.0 declarations aligned across Rust, SDKs, Web, desktop packages and container images. Third-party notices retain their original licenses.

CI now includes LocalApplication, builtins, authorization, RPC/ACP, real Python/TypeScript SDK calls, browser tests for groups and model service, and PostgreSQL Node routing under restricted runtime grants.

## Verification

Initial commit `da0d9bf` was pushed to main and started the [first Checks run](https://github.com/mosttt/ternilo/actions/runs/36443218372). Dependency checks passed remotely. Linux exposed a directory-picker test that asserted focus restoration before Radix's unmount timer ran; it now waits for the actual focus condition while preserving the assertion, and its six tests pass locally.

Local checks passed: 148 Web test files / 932 unit tests, seven rich-text tests, TypeScript, i18n, documentation links, production Web build, 30 deployment/release script tests, operations configuration, actionlint, workspace Clippy, dependency license/source/advisory checks, npm audit, and both SDK smoke tests.

The first local browser run used older binaries. Asset hashes and OIDC contracts showed they did not match current sources, so those results are excluded from current-version acceptance. Matching binaries, Rust tests and browser verification are being completed. A workflow file or a successful compilation alone does not establish release acceptance.

After rebuilding matching binaries, SDK file operations, five local/account/group/model-service browser scenarios, six OIDC/security/PWA/mobile scenarios, and two delivery plus SQLite/PostgreSQL recovery scenarios passed. Responsive group/model tests now wait for the application's viewport height to synchronize after resize before retaining all geometry and touch-target assertions. The second GitHub run passed Web/deployment/dependency checks and macOS Apple Silicon compilation; Rust and remaining platforms are still being verified.

CI also caches dependency build artifacts, using a pinned cache Action and the repository's Rust toolchain, with target and Linorun revision separation. Failed jobs retain dependency caches for the next run. Workspace code still undergoes normal Cargo compilation checks.

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
