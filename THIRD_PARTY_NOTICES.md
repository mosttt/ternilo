# 第三方声明 / Third-party notices

Ternilo 自有代码使用 [Apache-2.0](LICENSE)。第三方代码、素材与依赖保留原许可证，不因本项目的许可证变更而重新授权。

Ternilo's own code is licensed under [Apache-2.0](LICENSE). Third-party code, assets and dependencies retain their original licenses.

- Web UI 的基础组件沿用 [shadcn/ui](https://github.com/shadcn-ui/ui) 的组件模式与代码，保留其 [MIT 声明](licenses/web-interface.MIT)。The Web UI primitives adapt shadcn/ui components; its MIT notice is retained.
- Windows 沙箱使用 [zagens-windows-sandbox 0.8.9](https://crates.io/crates/zagens-windows-sandbox/0.8.9)，其源码包记录的上游提交为 `b5e6f375eabfb46ec374065b9aee4d2a913e315f`；保留该提交的 [MIT 声明](licenses/zagens-windows-sandbox.MIT)。The Windows sandbox uses this crate; the MIT notice comes from its recorded upstream revision.
- [Linorun](https://github.com/mosttt/linorun) 使用 `MIT OR Apache-2.0`，本项目选择 Apache-2.0；配套版本固定于 `.github/linorun-revision`。Linorun is dual-licensed; this project uses it under Apache-2.0 at the pinned revision.

Rust 与 Web 的依赖版本分别以 `Cargo.lock` 和 `web/package-lock.json` 为准。依赖许可证门禁配置在 `deny.toml`；该清单不将第三方依赖声明为 Ternilo 自有代码。重新分发时应保留相应依赖随附的版权、许可和 NOTICE 文件。

Rust and Web dependency versions are recorded in their lockfiles. Rust license checks are configured in `deny.toml`. Preserve the copyright, license and NOTICE files supplied by the relevant dependencies when redistributing them.
