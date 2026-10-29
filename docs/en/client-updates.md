# Update connected Ternilo clients from Server

In **User Settings → My machines**, choose **Update client** on a computer, or use **Client updates** above the list to select several computers. Server recommends the stable client release matching its own version. Configuration, conversations, computer identity and credentials are retained; the computer briefly goes offline during restart.

This feature updates official browser/CLI bundles, not desktop installers, containers or Work. First manually upgrade Server and computers to v0.2.30 or a later release supporting the independent maintenance channel. Keep the existing instance data, enrollment credential and service configuration; re-enrollment is unnecessary. Earlier clients do not have this channel, so updating Server alone cannot enable it.

Maintenance and task-execution protocols have independent versions. After a later Server upgrade changes the execution protocol, a computer can remain **Maintenance online**: tasks are unavailable, but remote updates can install the matching release and restore execution. No additional resident daemon is installed. The existing client must be running, reachable and authenticated. Data-epoch changes, incompatible maintenance protocols and damaged installations still require manual maintenance.

Computer cards and update dialogs periodically read cached Server state without waiting for computer RPCs. Maintenance is considered offline after 30 seconds without a report; open views refresh automatically.

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

The main systemd service needs a persistent unit file: a `systemd-run` transient service disappears after stopping and cannot be used as the restart target. The updater uses a separate transient scope, retaining the caller’s credentials, environment and filesystem context while surviving shutdown of the main service.

Add `--system` for a system-level service. The account must be able to control that service and create an independent transient update scope. For Supervisor, specify the exact program name:

```sh
ternilo updates enable --restart supervisor --service ternilo --supervisor-config "<supervisord.conf>" --data-dir "<instance-directory>"
```

Supervisor names must exactly match `supervisorctl status`. If it lists `ternilo:ternilo_00`, use `--service 'ternilo:ternilo_00'`, not just the group name `ternilo`. The client checks process ownership before downloading and again before installation.

For a macOS user LaunchAgent:

```sh
ternilo updates enable --restart launch-agent --service com.ternilo.client --data-dir "<instance-directory>"
```

`--service` is the plist's `Label`, not its filename. The default plist is `~/Library/LaunchAgents/<Label>.plist`; use `--launch-agent-plist "<absolute-plist-path>"` for another location. Both XML and binary plists are supported. The default domain is the current user's `gui/<uid>` login domain; add `--launchd-domain user` for an agent actually loaded in `user/<uid>`. Its plist must use `LimitLoadToSessionType` set to `Background`; a default GUI-session plist cannot be loaded directly in a user domain. Enable updates as the account running the agent, without switching accounts through `sudo`. This adapter does not manage system LaunchDaemons.

Before updating, Ternilo verifies the plist Label and the loaded agent's actual PID. Installation uses `bootout` to prevent KeepAlive from relaunching the old program, then `bootstrap` and `kickstart` with the original plist after installation or rollback. Arguments, environment variables and KeepAlive are not rewritten. The updater runs in a separate process group and survives unloading the main agent. A GUI domain requires an active login session; do not log out during installation.

Windows currently supports direct restart only. Do not select direct for a systemd/Supervisor/LaunchAgent-managed service: its manager may restart the old process during replacement. Managers should launch the stable entry point (`bin/ternilo` created by the official installer), not a fixed `releases/<version>-<target>/bin/ternilo` path.

Restart Ternilo once using its normal startup method, then refresh the Server dialog. If a manual upgrade changes the actual binary directory, such as a new `releases/<version>-<target>/bin`, run the same enable command with the new binary and restart to register that installation path. Server-driven upgrades update this metadata automatically and do not require re-enabling. One instance owns each installation. Separate instances need separate installations. Updates also check for other service processes using the same installation.

## Download, verification and activation

Server downloads the exact version's `client-updates.json`, `client-updates.json.sig` and platform bundle from the official GitHub Release. The release pipeline signs the manifest with a dedicated Ed25519 private key. It covers file size, SHA-256, minimum client version, supported Server versions and the data epoch. Server and clients contain only the public key; neither receives the production signing private key.

Server verifies and caches the bundle, then grants short-lived download access restricted to the computer and operation. The computer downloads it from Server over HTTPS and independently verifies the signature and bundle. Commands and progress use the independently versioned HTTPS maintenance endpoint, authenticated with the enrolled computer credential and independent of execution WebSocket compatibility. Production Server deployments should use HTTPS.

After download, the client waits for conversations, subtasks, queued work and background execution resources to become idle. Maintenance then rejects new work. An independent updater probes the new program before stopping the old service, installs the complete bundle, restarts it using the configured method and checks local service health. Local startup has a 120-second readiness window: a Supervisor RUNNING state alone does not establish that the new version is ready. Timeout still restores the previous program. Completion requires a healthy local service and a restored Server execution connection with the new version. A network outage alone does not trigger rollback. Downloading and waiting operations can be cancelled; installation cannot.

Idle persistent MCP/LSP services do not block updates. They shut down with the client and start again when a session needs them. Active tool calls and background jobs still block installation and are not forcibly stopped by an update.

Official script installations retain version directories. The updater installs a new `releases/<version>-<target>/bin`, switches the launcher and plugin link, and retains the launcher's no-argument browser behavior. Manually extracted flat installations use a complete executable backup and replacement. Neither layout replaces or migrates instance data. Startup failure restores the previous program.

Server defaults to a 2 GiB release cache in `client-update-cache` beside `workspace_root`. Set `TERNILO_SERVER_UPDATE_CACHE` to choose a writable directory. Containers need writable storage for that path. This rebuildable cache is not a data backup.

## Status, disabling and recovery

```sh
ternilo updates status --data-dir "<instance-directory>"
ternilo updates disable --data-dir "<instance-directory>"
```

Disabling prevents new updates without interrupting installation already in progress. Status is available locally when Server is disconnected. If an interrupted activation leaves an incomplete marker, service startup refuses to use a partially installed bundle. Stop the configured manager or direct instance first. For LaunchAgent, use `launchctl bootout 'gui/<uid>/<Label>'` (or `user/<uid>/<Label>` for a non-GUI domain); merely killing the process may cause KeepAlive to restart it immediately. Then run:

```sh
ternilo updates recover --data-dir "<instance-directory>"
```

An independent process restores the previous program and restarts the service. If installation had not begun, recovery clears the interrupted state and restarts the original program using saved settings. Recovery refuses to run alongside an active updater.

Recovery plans live at `<instance-directory>/data/updates/operations/<id>/apply.json`, independently of download caches, and belong in data backups. If the launcher is damaged, run the recovery command using cached `ternilo-update-helper` (`.exe` on Windows), or the `ternilo` binary from a freshly extracted official bundle. Do not overwrite the original instance data. Retain the original release, program backups and `data/updates/` until update or recovery is confirmed.

For preparation, permission or service-name errors, inspect the dialog diagnostics and correct the cause before retrying. Downgrades, existing target release directories and data-epoch changes require local maintenance.

## When an update waits or rolls back

Waiting applies to work whose completion can still be observed. If a client restart leaves a process record without a completion observer, the update reports the blocker instead of waiting indefinitely. Diagnostics identify the resource, session and run. The computer administrator must check the actual processes. Once the matching process is confirmed to have ended, cancel the update and stop the client, back up `<instance-directory>/runtime/execution-resources.json`, resolve only the confirmed record, then restart and retry. Do not delete the entire file merely because no conversation appears active; other records may still be valid.

After a startup rollback, keep the restored service running and inspect `ternilo status` and the service-manager logs. Initialization is not a crash. Avoid repeated retries or deleting the previous release. If the new program starts locally but exceeds the readiness window, investigate its startup time and logs. The update dialog distinguishes startup and reconnection from rollback results.

[Computer management](computers.md) · [Release signing](ci-release.md)
