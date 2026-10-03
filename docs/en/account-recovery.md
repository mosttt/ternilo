# Account passwords, email and multi-factor authentication

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

Signed-in accounts can change their own password. Forgotten passwords can be recovered through a verified email or by the operator procedure above. Enabled MFA remains required after password changes or recovery. If incorrect OIDC/Turnstile settings prevent login, use the separate [authentication-settings recovery](server-authentication.md) procedure.

## Email verification and recovery

The instance owner enables **Account email** under **Platform administration → Instance settings → Sign-in and verification** and configures the SMTP host, port, encryption, sender and optional credentials. STARTTLS on port 587 is the default; direct TLS on port 465 is supported. Certificates are verified and STARTTLS cannot downgrade. Unencrypted local relays are restricted to loopback addresses.

The SMTP password is encrypted and never returned by the settings API. A blank password preserves it only when host, port, encryption and username are unchanged. Links use the configured public Server root URL rather than the request Host; use HTTPS in production. Disabling account email stops delivery and email password resets without disabling ordinary login.

Signed-in users request verification under **User settings → General → Account**, open the email link, and confirm while signed in to the matching account. An email address or a matching claim from an external identity provider does not establish verification. Verification is optional for ordinary login; changing the account email requires verification again before recovery.

**Forgot password** sends a reset link only to a verified, active native account. Missing, unverified, OIDC-only and inactive accounts receive the same public response. Configured Turnstile protection also applies. Resetting revokes native/OIDC browser sessions on this site, preserves the account and resources, and requires a fresh sign-in. External IdP passwords and sessions are unaffected.

Links expire after 15 minutes and can succeed once. Resending invalidates the previous link of the same kind; any password update clears recovery links. The database stores SHA-256 digests rather than raw credentials. Link fragments are removed from the address bar after reading and are not sent in the initial HTTP request. Pending email verification is retained briefly in the current tab during OIDC sign-in.

Same-account deliveries of the same kind have a one-minute cooldown. Shared limits permit five email-related requests per source IP per minute and 100 per instance group per minute. Configure trusted proxy addresses correctly. Native password changes use no email quota. Each Server permits eight concurrent deliveries and a 20-second delivery timeout. On delivery failure, check SMTP settings and retry later. These are account-email protections, not general model/account quota controls.

Endpoints are `GET /api/v1/auth/email`, `POST /api/v1/auth/email/send`, `POST /api/v1/auth/email/verify`, `POST /api/v1/auth/password-recovery` and `POST /api/v1/auth/password-reset`. Verification requires site authentication. Recovery accepts `email` and optional `turnstile_token`; reset accepts `token` and `password`; confirmation accepts `token`. None return email credentials.


## Authenticator and recovery codes

Native accounts can open **User settings → General → Account → Multi-factor authentication**, enter their current password and scan the QR code with an authenticator (or enter the secret manually). Save the eight recovery codes shown once, confirm that they are saved, then enter the current six-digit authenticator code to enable MFA. Enrollment expires after ten minutes; starting again replaces a pending secret. Recovery codes cannot activate enrollment.

Password sign-ins and linked OIDC sign-ins then require an authenticator code or an unused recovery code. Enabling and disabling MFA revoke existing site browser sessions. After upstream OIDC authentication, the browser must complete the second-factor page before receiving site access credentials. Accounts with site MFA cannot use raw external OIDC access tokens to call site APIs. OIDC-only accounts use their identity provider's MFA policy; site enrollment requires a native password.

TOTP uses SHA-1, six digits and 30-second steps, allowing one adjacent step. A matched step can succeed once, including the code used to enable MFA. Keep device time accurate and wait for the next code after using one. Five incorrect attempts suspend authenticator verification for five minutes; unused recovery codes can still restore access. Each recovery code succeeds once. Settings show the remaining count without revealing the original codes. To replace an authenticator or replenish recovery codes, disable MFA with the current password and a code, then enroll again.

Password changes, operator password resets and email recovery retain MFA. Computer credentials, model keys and service credentials keep their independent authorization and revocation rules. Passkeys are not available.

The Server database encrypts authenticator secrets and stores only recovery-code digests; responses are not cached. If both the authenticator and all recovery codes are lost, an operator with access to the Server's private configuration and master key can run:

```bash
./bin/ternilo-server admin reset-mfa \
  --config-dir /path/to \
  --username owner
```

This removes MFA and revokes site browser sessions, preserving the password, identity, resources and OIDC link. It does not unban accounts or restore removed accounts. Enroll again after signing in. Recovery is audited; no public endpoint bypasses MFA.

Endpoints are `GET /api/v1/auth/mfa`, `POST /api/v1/auth/mfa/setup`, `POST /api/v1/auth/mfa/enable` and `POST /api/v1/auth/mfa/disable`. Setup accepts `current_password`; enable additionally requires `generation` and `code`; disable additionally requires `code`. Native login accepts `mfa_code`. OIDC callbacks can return a short-lived `mfa_challenge`; `POST /auth/mfa` accepts `challenge` and `code` to finish authentication. Pending challenges cannot access resources.
