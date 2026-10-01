# Recover a native account password

English · [简体中文](../zh-CN/account-recovery.md)

An operator who can read the Server's private configuration can reset an existing native account password. The account ID, roles, personal space, workspaces, sessions, computers and model grants retain their ownership. This does not create accounts or add native credentials to OIDC-only accounts.

Run on the Server host:

```bash
./bin/ternilo-server admin reset-password \
  --config-dir /path/to \
  --username owner
```

The terminal hides the new password and asks for confirmation. Registration password rules apply: 8–1024 bytes, including any spaces. Sign in with the existing username and the new password; no database replacement or Server restart is required.

For automation, explicitly read from standard input, for example from a protected temporary file:

```bash
./bin/ternilo-server admin reset-password \
  --config-dir /path/to \
  --username owner \
  --password-stdin < /path/to/protected-password-file
```

One trailing LF or CRLF is removed; other characters are retained. Non-interactive invocation without this option is rejected. Passwords are not accepted as command-line arguments and are excluded from output and audit records. Restrict configuration and password-file access to operators.

The password update and revocation of existing native/OIDC site browser sessions commit together. A native login verified before the reset cannot issue a new session afterward. Old sessions fail subsequent requests, and live browser connections request sign-in after their next authorization check. Accepted tasks are not automatically interrupted.

Computer credentials, model keys and OIDC bindings are retained. External IdP credentials and organization sessions remain under IdP control. Banned and pending accounts retain their status; removed accounts cannot be recovered. Use [account administration](../zh-CN/platform-management.md) for access-state changes.

This is local operator recovery, not email recovery, self-service password reset or MFA. If incorrect OIDC/Turnstile settings prevent login, use the separate [authentication-settings recovery](../zh-CN/server-authentication.md) procedure.
