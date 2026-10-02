# Identifiers and credentials

Public credential formats and browser session IDs use `ter_` followed by a purpose code. `ter` identifies Ternilo; the remainder retains the complete random value or digest.

| Prefix | Purpose |
|---|---|
| `ter_s_` | Public browser-session ID for inspection/revocation; not a login credential |
| `ter_pc_` | System-generated permanent computer instance ID; not an access credential |
| `ter_a_` | Native account access credential |
| `ter_o_` | Ternilo-issued OIDC access credential |
| `ter_r_` | OIDC refresh credential |
| `ter_b_` | Initial Server setup credential |
| `ter_n_` | Node connection credential |
| `ter_e_` | One-time Node enrollment credential |
| `ter_w_` | Worker credential |
| `ter_i_` | Account/team invitation credential |
| `ter_m_` | Model-service API key |
| `ter_d_` | Model-service device access credential |
| `ter_c_` | Model-service device authorization code |

Prefixes identify a purpose; they do not replace identity, expiry, authorization or revocation checks. Public session IDs cannot authenticate. Use credentials only with their designated service. Short IDs and API-key prefixes displayed in the interface are identifiers, not complete credentials.
