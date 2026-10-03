# Change or recover a native account password

English · [简体中文](../zh-CN/account-recovery.md)

Signed-in native accounts can use **User settings → General → Account → Change password**, supplying the current password and confirming the new one. Success revokes every native/OIDC browser sign-in on this site, including the current page. The password update and revocation commit together; incorrect credentials or a concurrent password change cannot overwrite the current password. OIDC-only accounts do not see this option.

`POST /api/v1/auth/password` uses the current account's browser authentication and accepts `current_password` and `new_password` in JSON, without a target account parameter. The response contains no password. External identity-provider sessions, computer credentials, model keys and service-account credentials are unaffected.

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

Signed-in accounts can change their own password. Forgotten passwords can be recovered through a verified email or by the operator procedure above. MFA is not yet available. If incorrect OIDC/Turnstile settings prevent login, use the separate [authentication-settings recovery](../zh-CN/server-authentication.md) procedure.

## Email verification and recovery

The instance owner enables **Account email** under **Platform administration → Instance settings → Sign-in and verification** and configures the SMTP host, port, encryption, sender and optional credentials. STARTTLS on port 587 is the default; direct TLS on port 465 is supported. Certificates are verified and STARTTLS cannot downgrade. Unencrypted local relays are restricted to loopback addresses.

The SMTP password is encrypted and never returned by the settings API. A blank password preserves it only when host, port, encryption and username are unchanged. Links use the configured public Server root URL rather than the request Host; use HTTPS in production. Disabling account email stops delivery and email password resets without disabling ordinary login.

Signed-in users request verification under **User settings → General → Account**, open the email link, and confirm while signed in to the matching account. An email address or a matching claim from an external identity provider does not establish verification. Verification is optional for ordinary login; changing the account email requires verification again before recovery.

**Forgot password** sends a reset link only to a verified, active native account. Missing, unverified, OIDC-only and inactive accounts receive the same public response. Configured Turnstile protection also applies. Resetting revokes native/OIDC browser sessions on this site, preserves the account and resources, and requires a fresh sign-in. External IdP passwords and sessions are unaffected.

Links expire after 15 minutes and can succeed once. Resending invalidates the previous link of the same kind; any password update clears recovery links. The database stores SHA-256 digests rather than raw credentials. Link fragments are removed from the address bar after reading and are not sent in the initial HTTP request. Pending email verification is retained briefly in the current tab during OIDC sign-in.

Same-account deliveries of the same kind have a one-minute cooldown. Shared limits permit five email-related requests per source IP per minute and 100 per instance group per minute. Configure trusted proxy addresses correctly. Native password changes use no email quota. Each Server permits eight concurrent deliveries and a 20-second delivery timeout. On delivery failure, check SMTP settings and retry later. These are account-email protections, not general model/account quota controls.

Endpoints are `GET /api/v1/auth/email`, `POST /api/v1/auth/email/send`, `POST /api/v1/auth/email/verify`, `POST /api/v1/auth/password-recovery` and `POST /api/v1/auth/password-reset`. Verification requires site authentication. Recovery accepts `email` and optional `turnstile_token`; reset accepts `token` and `password`; confirmation accepts `token`. None return email credentials.
