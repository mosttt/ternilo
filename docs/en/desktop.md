# Desktop application

Desktop provides a native Tauri window over the same local application used by CLI and the browser. Install the package for your platform from GitHub Releases, or build from a matching source checkout:

```bash
npm --prefix web ci
npm --prefix web run build
cargo run --locked -p ternilo-desktop
```

## Service lifecycle

The window connects to the authenticated local service. Closing the window can leave the background service running so remote access and tasks remain available. Inspect or stop the service with:

```text
ternilo status --data-dir /path/to/ternilo-data
ternilo stop --data-dir /path/to/ternilo-data
```

Use the data directory configured for that instance. A browser and desktop window can share one service; two independent services must use separate directories and ports. See [multiple instances](remote-access.md#multiple-instances-on-one-computer). Desktop's single-instance window behavior does not create a new Node on every launch.

Without `--listen` or `TERNILO_DESKTOP_LISTEN`, Desktop reads the listening address from the instance's `config.json`; the first launch defaults to `127.0.0.1:3210`. Explicit overrides apply only to that launch and do not rewrite an existing configuration. Native capabilities require `127.0.0.1`. Background services launched by Desktop write their log to `runtime/service.log` inside the data directory.

Release Windows builds use the GUI subsystem, so launching the desktop app does not create a controlling console window. The CLI and Server retain console behavior. The child background service is launched without a visible console.

## Native integration

Native capabilities include directory selection, opening the configuration directory, window management, notifications and supported deep links. The workspace link form is:

```text
ternilo://workspace?path=<percent-encoded-absolute-path>
```

The path identifies a local directory and must be properly encoded. Remote access still uses Server registration and credentials; a native window does not replace Server authentication or expand file permissions.

Windows execution requires `ternilo-sandbox-windows.exe`. Release tooling builds and bundles the matching helper; keep it with portable client executables. It enforces the process sandbox and is not a second user-facing app. Source builds must prepare the sidecar as described by the platform packaging workflow.

## Packaging and updates

Build Web first, then use `cargo tauri build` from `apps/ternilo-desktop` on the target platform. GitHub packages Linux DEB/AppImage, Windows x86_64 NSIS/MSI, Windows ARM64 NSIS EXE and separate Intel/Apple Silicon DMGs. License files are included in application resources.

Automatic desktop updates are disabled until release signing and update delivery are configured. Current installers are unsigned/unnotarized. Operating-system warnings and platform installation checks must be handled as part of release acceptance. Linux portable delivery does not include every host dependency; consult [packaging](release-packaging.md) and [security](security.md).
