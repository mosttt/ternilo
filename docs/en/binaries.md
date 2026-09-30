# Programs in the downloads

[简体中文](../zh-CN/binaries.md)

The client product and command are both `ternilo`; portable client archives use `ternilo-<version>-<target>`. Windows archives use ZIP; Linux/macOS CLI archives use tar.gz. Desktop installers use EXE/MSI, DMG and DEB/AppImage respectively.

| Executable | Purpose | Usage |
|---|---|---|
| `ternilo` / `ternilo.exe` | Local client and web entry point, optionally connected to Server | Launch directly or run `ternilo serve` |
| `ternilo-plugin` / `ternilo-plugin.exe` | Extension developer CLI for publisher keys, WASM componentization, signing and verification | Not needed for ordinary use; begin with `ternilo-plugin --help` |
| `ternilo-sandbox-windows.exe` | Restricted Windows tool execution and process-tree cleanup | Keep beside `ternilo.exe`; the client invokes it automatically |
| `ternilo-server` / `ternilo-server.exe` | Remote accounts, computer connections, collaboration and model service | Download the separate Server archive or use the public Docker image |

Run the portable programs from `bin`, or add that directory to PATH. Do not move only `ternilo.exe` without its Windows sandbox runner. The runner needs no independent startup, port or Server connection. Complete read isolation is not currently claimed on Windows.

```sh
ternilo-plugin --help
ternilo-plugin sign --help
ternilo-plugin verify --bundle extension.json --publisher publisher.json
```

Verification checks signatures, digests, host limits and runtime compilation; it does not install an extension. Installation and publisher trust are managed in Ternilo's extension settings. Keep signing private keys outside distributable bundles and repositories.

Ordinary users only start the main client. Desktop installers place their own supporting executables; they do not require manual copying from the portable archive.
