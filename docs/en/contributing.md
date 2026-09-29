# Development and validation

Ternilo uses Rust edition 2024 and the toolchain pinned in `rust-toolchain.toml`. Check out Linorun beside Ternilo at the revision in `.github/linorun-revision`. CI and releases use that revision; local uncommitted changes in Linorun are not release inputs. Linux command execution requires Bubblewrap, and file search requires ripgrep.

From the Ternilo source directory:

```bash
npm --prefix web ci
npm --prefix web run build
cargo build --locked -p ternilo -p ternilo-server
```

The Rust binaries embed Web assets. Rebuild Web before building binaries used for browser validation. `web/rich-text.source.js` is bundled by Vite; do not maintain a second manually generated renderer. Keep lockfiles and the pinned toolchain consistent.

## Scoped checks

Select tests for the changed modules, then run actual browser flows for UI changes:

```bash
cargo fmt --all -- --check
cargo test --locked -p ternilo-local --lib
npm --prefix web run typecheck
npm --prefix web run verify:i18n
npm --prefix web run test:unit
npm --prefix web run verify:docs
python3 deploy/docker/tests/deployment-tools.test.py
```

Broader changes to shared infrastructure or release candidates justify workspace tests, Clippy with warnings denied, documentation compilation and the appropriate integration gates. Run `cargo deny check` and npm audit for dependency changes. Do not hide new vulnerabilities with blanket exclusions.

`npm --prefix web run test:browser` exercises real Chromium interactions. Native desktop smoke and installation recovery are separate checks. Browser results require matching binaries and Web assets, plus checks of rendering, core interactions, console errors and network requests. A component test alone does not validate a complete UI flow.

PostgreSQL tests must use a disposable database and the repository's restricted-role setup. Never point integration tests at ordinary user data. Docker release acceptance uses isolated images, volumes and explicit output paths. A temporary development workaround must not silently become a standard CI requirement.

## Documentation and delivery

Formal user/maintenance guides live in paired `docs/zh-CN/` and `docs/en/` files. `docs/development/` currently contains Chinese development records only and is excluded from binary packages. Update docs with behavior changes and validate relative links and language coverage.

Keep generated Web output synchronized with its source. Commit only intended source, documentation and generated release inputs; never runtime credentials or databases. See [CI](ci-release.md), [package generation](release-packaging.md) and [architecture](architecture.md).
