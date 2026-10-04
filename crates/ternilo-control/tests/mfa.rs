use ternilo_control::{ControlStore, NativeRegistration, SecretCipher};

const PASSWORD: &str = "mfa-contract-password";
const START: u64 = 1_700_000_000_000;

fn authenticator(secret: &str) -> totp_rs::Totp {
    totp_rs::Builder::new()
        .with_secret(totp_rs::Secret::try_from_base32(secret).unwrap())
        .with_skew(1)
        .build()
        .unwrap()
}

async fn verify(
    store: &ControlStore,
    password: &str,
    code: Option<&str>,
    now: u64,
) -> Result<ternilo_control::NativeSessionGrant, ternilo_protocol::HarnessError> {
    let credentials = store.verify_native_credentials("owner", password).await?;
    let credentials = store.verify_native_mfa(credentials, code, now).await?;
    store.create_native_browser_session(credentials, now).await
}

#[expect(
    clippy::too_many_lines,
    reason = "Exercise setup, revocation, replay, rate limits and password recovery for the same factor."
)]
async fn contract(url: &str, migration: Option<&str>) {
    let store = ControlStore::connect(url, migration, SecretCipher::from_key([91; 32]), 4)
        .await
        .unwrap();
    let owner = store
        .initialize_owner(
            &NativeRegistration {
                email: "mfa@example.test".into(),
                username: "owner".into(),
                password: PASSWORD.into(),
            },
            START,
        )
        .await
        .unwrap();
    let actor = &owner.session.user;
    let before = store
        .verify_native_credentials("owner", PASSWORD)
        .await
        .unwrap();
    assert!(!store.mfa_status(actor).await.unwrap().enabled);
    assert!(
        store
            .begin_mfa_enrollment(actor, "incorrect-password", START)
            .await
            .is_err()
    );
    let replaced = store
        .begin_mfa_enrollment(actor, PASSWORD, START + 1)
        .await
        .unwrap();
    let setup = store
        .begin_mfa_enrollment(actor, PASSWORD, START + 2)
        .await
        .unwrap();
    assert_ne!(setup.secret, replaced.secret);
    assert_eq!(setup.recovery_codes.len(), 8);
    assert!(setup.qr_code.starts_with("data:image/png;base64,"));
    assert!(
        setup
            .recovery_codes
            .iter()
            .all(|code| code.starts_with("ter_mr_"))
    );
    let totp = authenticator(&setup.secret);
    let code = totp.generate(START / 1000).to_string();
    assert!(
        store
            .activate_mfa(actor, PASSWORD, &replaced.generation, &code, START + 3)
            .await
            .is_err()
    );
    assert!(
        store
            .activate_mfa(
                actor,
                PASSWORD,
                &setup.generation,
                &setup.recovery_codes[0],
                START + 3
            )
            .await
            .is_err(),
        "setup requires the authenticator, not a recovery code"
    );
    store
        .activate_mfa(actor, PASSWORD, &setup.generation, &code, START + 4)
        .await
        .unwrap();
    assert!(store.mfa_status(actor).await.unwrap().enabled);
    assert_eq!(
        store
            .mfa_status(actor)
            .await
            .unwrap()
            .recovery_codes_remaining,
        8
    );
    assert!(
        store
            .authenticate_native_session(&owner.access_token, START + 5)
            .await
            .is_err()
    );
    assert!(
        store
            .create_native_browser_session(before, START + 5)
            .await
            .is_err(),
        "a proof verified before MFA enablement cannot create a session"
    );
    assert!(
        store
            .create_browser_session(actor.clone(), START + 5)
            .await
            .is_err(),
        "trusted identity issuance does not silently bypass MFA"
    );
    assert!(verify(&store, PASSWORD, None, START + 5).await.is_err());
    assert!(
        verify(&store, PASSWORD, Some(&code), START + 5)
            .await
            .is_err(),
        "the setup code cannot be replayed for login"
    );
    let next = START + 60_000;
    let code = totp.generate(next / 1000).to_string();
    let login = verify(&store, PASSWORD, Some(&code), next).await.unwrap();
    assert_eq!(login.session.user, *actor);
    assert_eq!(
        login.session.personal_project_id,
        owner.session.personal_project_id
    );
    assert!(
        verify(&store, PASSWORD, Some(&code), next + 1)
            .await
            .is_err()
    );
    let later = START + 120_000;
    for _ in 0..5 {
        assert!(
            verify(&store, PASSWORD, Some("invalid"), later)
                .await
                .is_err()
        );
    }
    assert!(
        verify(
            &store,
            PASSWORD,
            Some(&totp.generate(later / 1000).to_string()),
            later
        )
        .await
        .is_err(),
        "correct TOTP waits out the persisted failure limit"
    );
    let recovery = verify(&store, PASSWORD, Some(&setup.recovery_codes[0]), later)
        .await
        .unwrap();
    assert_eq!(recovery.session.user, *actor);
    assert_eq!(
        store
            .mfa_status(actor)
            .await
            .unwrap()
            .recovery_codes_remaining,
        7
    );
    assert!(
        verify(&store, PASSWORD, Some(&setup.recovery_codes[0]), later + 1)
            .await
            .is_err()
    );
    let (first, second) = tokio::join!(
        verify(&store, PASSWORD, Some(&setup.recovery_codes[1]), later + 2),
        verify(&store, PASSWORD, Some(&setup.recovery_codes[1]), later + 2),
    );
    assert_ne!(
        first.is_ok(),
        second.is_ok(),
        "a recovery code succeeds once across concurrent requests"
    );
    store
        .reset_native_password("owner", "recovered-mfa-password", later + 3)
        .await
        .unwrap();
    assert!(store.mfa_status(actor).await.unwrap().enabled);
    assert!(
        verify(&store, "recovered-mfa-password", None, later + 4)
            .await
            .is_err(),
        "password recovery retains MFA"
    );
    let authenticated = verify(
        &store,
        "recovered-mfa-password",
        Some(&setup.recovery_codes[2]),
        later + 5,
    )
    .await
    .unwrap();
    let old_proof = store
        .verify_native_credentials("owner", "recovered-mfa-password")
        .await
        .unwrap();
    let old_proof = store
        .verify_native_mfa(old_proof, Some(&setup.recovery_codes[3]), later + 6)
        .await
        .unwrap();
    store
        .disable_mfa(
            actor,
            "recovered-mfa-password",
            &setup.recovery_codes[4],
            later + 7,
        )
        .await
        .unwrap();
    assert!(
        store
            .authenticate_native_session(&authenticated.access_token, later + 8)
            .await
            .is_err()
    );
    assert!(!store.mfa_status(actor).await.unwrap().enabled);
    assert!(
        verify(&store, "recovered-mfa-password", None, later + 9)
            .await
            .is_ok()
    );
    let again = store
        .begin_mfa_enrollment(actor, "recovered-mfa-password", later + 10)
        .await
        .unwrap();
    let again_code = authenticator(&again.secret)
        .generate((later + 10) / 1000)
        .to_string();
    store
        .activate_mfa(
            actor,
            "recovered-mfa-password",
            &again.generation,
            &again_code,
            later + 11,
        )
        .await
        .unwrap();
    assert!(
        store
            .create_native_browser_session(old_proof, later + 12)
            .await
            .is_err(),
        "second-factor proof is bound to the active configuration generation"
    );
    oidc_contract(&store, actor, &again, url, migration, later + 1000).await;
    let audit: Vec<String> = sqlx::query_scalar(
        "SELECT metadata FROM control_platform_audit WHERE action LIKE 'account.mfa.%'",
    )
    .fetch_all(store.database().pool())
    .await
    .unwrap();
    for record in audit {
        assert!(
            !record.contains(&setup.secret)
                && !record.contains("ter_mr_")
                && !record.contains(PASSWORD)
        );
    }
    store.database().close().await;
}

#[expect(
    clippy::too_many_lines,
    reason = "Check OIDC challenge/refresh/binding and cipher maintenance without bypassing the active factor."
)]
async fn oidc_contract(
    store: &ControlStore,
    actor: &ternilo_control::ControlUser,
    setup: &ternilo_control::MfaEnrollment,
    url: &str,
    migration: Option<&str>,
    now: u64,
) {
    let principal = ternilo_control::OidcPrincipal {
        issuer: "https://mfa.example.test".into(),
        subject: "owner".into(),
        email: Some("mfa@example.test".into()),
        display_name: None,
    };
    store
        .link_native_oidc(actor, &principal, now)
        .await
        .unwrap();
    let identity = ternilo_control::OidcSessionIdentity {
        principal,
        nonce: "mfa-oidc-nonce".into(),
        upstream_refresh_token: Some("private-upstream-refresh".into()),
    };
    assert!(
        store.require_external_oidc_allowed(actor).await.is_err(),
        "a raw upstream identity cannot bypass site MFA"
    );
    assert!(
        store
            .create_oidc_session(&identity, "mfa-binding", now + 600_000, now)
            .await
            .is_err()
    );
    let challenge = store
        .begin_oidc_mfa(&identity, "mfa-binding", now + 600_000, now)
        .await
        .unwrap()
        .unwrap();
    assert!(challenge.mfa_challenge.starts_with("ter_mc_"));
    assert!(
        store
            .authenticate_oidc_session(&challenge.mfa_challenge, &["mfa-binding"], now)
            .await
            .is_err()
    );
    assert!(
        store
            .complete_oidc_mfa(
                &challenge.mfa_challenge,
                &setup.recovery_codes[0],
                "other-binding",
                now + 1
            )
            .await
            .is_err()
    );
    let grant = store
        .complete_oidc_mfa(
            &challenge.mfa_challenge,
            &setup.recovery_codes[0],
            "mfa-binding",
            now + 2,
        )
        .await
        .unwrap();
    assert!(
        store
            .complete_oidc_mfa(
                &challenge.mfa_challenge,
                &setup.recovery_codes[1],
                "mfa-binding",
                now + 3
            )
            .await
            .is_err()
    );
    assert_eq!(
        store
            .authenticate_oidc_session(&grant.access_token, &["mfa-binding"], now + 3)
            .await
            .unwrap()
            .0
            .subject,
        "owner"
    );
    let refresh = store
        .oidc_refresh_session(
            grant.refresh_token.as_ref().unwrap(),
            "mfa-binding",
            now + 4,
        )
        .await
        .unwrap();
    let rotated = store
        .replace_oidc_session(
            &refresh.identity,
            "mfa-binding",
            now + 600_000,
            (grant.refresh_token.as_ref().unwrap(), refresh.expires_at_ms),
            now + 5,
        )
        .await
        .unwrap();
    assert!(
        store
            .authenticate_oidc_session(&rotated.access_token, &["mfa-binding"], now + 6)
            .await
            .is_ok()
    );
    // A token minted before an identity is linked has no second-factor assurance.
    sqlx::query("DELETE FROM control_oidc_mfa_assurances WHERE user_id=$1")
        .bind(actor.user_id.as_str())
        .execute(store.database().pool())
        .await
        .unwrap();
    assert!(
        store
            .authenticate_oidc_session(&rotated.access_token, &["mfa-binding"], now + 7)
            .await
            .is_err()
    );
    assert!(
        store
            .oidc_refresh_session(
                rotated.refresh_token.as_ref().unwrap(),
                "mfa-binding",
                now + 7
            )
            .await
            .is_err()
    );
    assert!(
        store
            .replace_oidc_session(
                &identity,
                "mfa-binding",
                now + 600_000,
                (
                    rotated.refresh_token.as_ref().unwrap(),
                    refresh.expires_at_ms
                ),
                now + 7
            )
            .await
            .is_err()
    );
    let cancelled = store
        .begin_oidc_mfa(&identity, "mfa-binding", now + 600_000, now + 8)
        .await
        .unwrap()
        .unwrap();
    store
        .reset_native_password("owner", "second-recovered-password", now + 9)
        .await
        .unwrap();
    assert!(
        store
            .complete_oidc_mfa(
                &cancelled.mfa_challenge,
                &setup.recovery_codes[1],
                "mfa-binding",
                now + 10
            )
            .await
            .is_err(),
        "recovery revokes unfinished OIDC authentication"
    );
    assert!(
        verify(store, "second-recovered-password", None, now + 10)
            .await
            .is_err()
    );
    let pending = store
        .begin_oidc_mfa(&identity, "mfa-binding", now + 600_000, now + 11)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        ControlStore::rotate_secret_master_key(
            migration.unwrap_or(url),
            &SecretCipher::from_key([91; 32]),
            &SecretCipher::from_key([92; 32])
        )
        .await
        .unwrap(),
        2
    );
    let reopened = ControlStore::connect(url, migration, SecretCipher::from_key([92; 32]), 2)
        .await
        .unwrap();
    let approved = reopened
        .complete_oidc_mfa(
            &pending.mfa_challenge,
            &setup.recovery_codes[1],
            "mfa-binding",
            now + 12,
        )
        .await
        .unwrap();
    assert!(
        reopened
            .authenticate_oidc_session(&approved.access_token, &["mfa-binding"], now + 13)
            .await
            .is_ok()
    );
    assert!(
        verify(
            &reopened,
            "second-recovered-password",
            Some(&setup.recovery_codes[2]),
            now + 14
        )
        .await
        .is_ok()
    );
    assert_eq!(
        reopened.reset_native_mfa("OWNER", now + 15).await.unwrap(),
        *actor
    );
    assert!(!reopened.mfa_status(actor).await.unwrap().enabled);
    assert!(
        reopened
            .authenticate_oidc_session(&approved.access_token, &["mfa-binding"], now + 16)
            .await
            .is_err()
    );
    assert!(
        verify(&reopened, "second-recovered-password", None, now + 16)
            .await
            .is_ok()
    );
    reopened.database().close().await;
}

#[tokio::test]
async fn sqlite_mfa_requires_fresh_one_time_codes_and_retains_account_identity() {
    let directory = tempfile::tempdir().unwrap();
    contract(
        &format!(
            "sqlite://{}?mode=rwc",
            directory.path().join("mfa.sqlite3").display()
        ),
        None,
    )
    .await;
}

#[path = "support/postgres.rs"]
mod postgres_runtime;

#[tokio::test]
#[ignore = "requires TERNILO_TEST_DATABASE_URL pointing to a disposable PostgreSQL database"]
async fn postgres_mfa_uses_runtime_grants_and_consumes_codes_once() {
    let admin_url = std::env::var("TERNILO_TEST_DATABASE_URL").unwrap();
    assert!(admin_url.contains("ternilo_control_test"));
    let admin = sqlx::PgPool::connect(&admin_url).await.unwrap();
    sqlx::raw_sql("DROP SCHEMA IF EXISTS public CASCADE; CREATE SCHEMA public;")
        .execute(&admin)
        .await
        .unwrap();
    postgres_runtime::prepare_role(&admin, "ternilo_mfa_test", "mfa-password").await;
    let mut runtime = admin_url
        .parse::<sqlx::any::AnyConnectOptions>()
        .unwrap()
        .database_url;
    runtime.set_username("ternilo_mfa_test").unwrap();
    runtime.set_password(Some("mfa-password")).unwrap();
    contract(runtime.as_str(), Some(&admin_url)).await;
    admin.close().await;
}
