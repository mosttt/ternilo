# Update connected Ternilo clients from Server

In **User Settings → My machines**, choose **Update client** on a computer, or use **Client updates** above the list to select several computers. Server recommends the stable client release matching its own version. Configuration, conversations, computer identity and credentials are retained; the computer briefly goes offline during restart.

This feature updates official browser/CLI bundles, not desktop installers, containers or Work. First manually upgrade Server and connected executors together to versions supporting Executor protocol 49. Remote updates then operate within a compatible protocol and data epoch. Protocol or data-format changes require coordinated maintenance.

## Enable on the computer once

The computer owner must explicitly enable updates using the account running Ternilo. Select the actual restart method. Use the existing instance directory with `--data-dir`, or omit it for the default directory.

For a directly launched service:

```sh
ternilo updates enable --restart direct --data-dir "<instance-directory>"
```

For a user systemd service:

```sh
ternilo updates enable --restart systemd --service ternilo.service --data-dir "<instance-directory>"
```

The main systemd service needs a persistent unit file: a `systemd-run` transient service disappears after stopping and cannot be used as the restart target. The updater itself uses a separate transient unit so stopping the main service does not kill it.

Add `--system` for a system-level service. The account must be able to control that service and create an independent transient update unit. For Supervisor, specify the exact program name:

```sh
ternilo updates enable --restart supervisor --service ternilo --supervisor-config "<supervisord.conf>" --data-dir "<instance-directory>"
```

Windows currently supports direct restart only. Do not select direct for a systemd/Supervisor-managed service: its manager may restart the old process during replacement. Managers should launch the stable entry point (`bin/ternilo` created by the official installer), not a fixed `releases/<version>-<target>/bin/ternilo` path.

Restart Ternilo once using its normal startup method, then refresh the Server dialog. One instance owns each installation. Separate instances need separate installations. Updates also check for other service processes using the same installation.

## Download, verification and activation

Server downloads the exact version's `client-updates.json`, `client-updates.json.sig` and platform bundle from the official GitHub Release. The release pipeline signs the manifest with a dedicated Ed25519 private key. It covers file size, SHA-256, minimum client version, supported Server versions and the data epoch. Server and clients contain only the public key; neither receives the production signing private key.

Server verifies and caches the bundle, then grants short-lived download access restricted to the computer and operation. The computer downloads it from Server over HTTPS and independently verifies the signature and bundle. Commands and progress use the existing authenticated WebSocket connection. Production Server deployments should use HTTPS.

After download, the client waits for conversations, subtasks, queued work and background execution resources to become idle. Maintenance then rejects new work. An independent updater probes the new program before stopping the old service, installs the complete bundle, restarts it using the configured method and checks local service health. Completion requires reconnecting to Server with the new version. A network outage alone does not trigger rollback. Downloading and waiting operations can be cancelled; installation cannot.

Official script installations retain version directories. The updater installs a new `releases/<version>-<target>/bin`, switches the launcher and plugin link, and retains the launcher's no-argument browser behavior. Manually extracted flat installations use a complete executable backup and replacement. Neither layout replaces or migrates instance data. Startup failure restores the previous program.

Server defaults to a 2 GiB release cache in `client-update-cache` beside `workspace_root`. Set `TERNILO_SERVER_UPDATE_CACHE` to choose a writable directory. Containers need writable storage for that path. This rebuildable cache is not a data backup.

## Status, disabling and recovery

```sh
ternilo updates status --data-dir "<instance-directory>"
ternilo updates disable --data-dir "<instance-directory>"
```

Disabling prevents new updates without interrupting installation already in progress. Status is available locally when Server is disconnected. If an interrupted activation leaves an incomplete marker, service startup refuses to use a partially installed bundle. Stop the configured systemd/Supervisor service or direct instance, then run:

```sh
ternilo updates recover --data-dir "<instance-directory>"
```

An independent process restores the previous program and restarts the service. If the launcher is damaged, use `ternilo-update-helper` (`.exe` on Windows) in this operation's cache directory to run the same `updates recover` command. Retain the original release, backup directory and `<instance-directory>/cache/client-updates/` until update or recovery is confirmed.

For preparation, permission or service-name errors, inspect the dialog diagnostics and correct the cause before retrying. Downgrades, existing target release directories and data-epoch changes require local maintenance.

[Computer management](computers.md) · [Release signing](ci-release.md)
