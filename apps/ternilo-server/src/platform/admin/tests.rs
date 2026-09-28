use std::{collections::BTreeSet, sync::Arc};

use salvo_core::{
    Service,
    test::{ResponseExt, TestClient},
};
use serde_json::{Value, json};
use ternilo_cloud::{CloudSessionEventFeed, CloudStore};
use ternilo_control::{
    ControlStore, ControlUser, NativeRegistration, NativeSessionGrant, OidcPrincipal, SecretCipher,
    TenantQuota,
};

use super::*;
use crate::platform::{edge::EdgeGateway, state::AppState};

#[path = "registration_tests.rs"]
mod registration_tests;
#[path = "security_tests.rs"]
mod security_tests;

struct Fixture {
    store: ControlStore,
    state: AppState,
    service: Service,
    owner: NativeSessionGrant,
    shutdown: tokio::sync::watch::Sender<bool>,
}

impl Fixture {
    async fn new() -> Self {
        let now = now_ms().unwrap();
        let store =
            ControlStore::connect("sqlite::memory:", None, SecretCipher::from_key([17; 32]), 1)
                .await
                .unwrap();
        let owner = store
            .initialize_owner(
                &NativeRegistration {
                    email: "owner@example.test".to_owned(),
                    username: "owner".to_owned(),
                    password: "owner-test-password".to_owned(),
                },
                now,
            )
            .await
            .unwrap();
        store
            .set_instance_mode(&owner.session.user, InstanceMode::MultiUser, 1, now)
            .await
            .unwrap();
        let cloud = CloudStore::from_database(store.database().clone())
            .await
            .unwrap();
        let catalog = ternilo_cloud::catalog().unwrap();
        let policy = crate::platform::load_worker_policy(None, &catalog).unwrap();
        let (shutdown, receiver) = tokio::sync::watch::channel(false);
        let state = AppState {
            cloud_events: CloudSessionEventFeed::from_database(store.database().clone())
                .await
                .unwrap(),
            edge: Arc::new(EdgeGateway::new(store.edge_store()).await.unwrap()),
            store: store.clone(),
            cloud,
            security: Arc::default(),
            setup_token_hash: None,
            managed_execution_enabled: true,
            shutdown: receiver,
            worker_policy: Arc::new(policy),
            catalog: Arc::new(catalog),
        };
        let service = Service::new(crate::platform::web_router(state.clone()));
        Self {
            store,
            state,
            service,
            owner,
            shutdown,
        }
    }

    async fn account(&self, name: &str, role: PlatformRole) -> NativeSessionGrant {
        let now = now_ms().unwrap();
        let user = self
            .store
            .upsert_user(
                &OidcPrincipal {
                    issuer: "https://identity.example.test".to_owned(),
                    subject: name.to_owned(),
                    email: Some(format!("{name}@example.test")),
                    display_name: Some(name.to_owned()),
                },
                &format!("test-{name}"),
                now,
            )
            .await
            .unwrap();
        if role != PlatformRole::User {
            self.store
                .set_account_role(&self.owner.session.user, &user.user_id, role, 1, now)
                .await
                .unwrap();
        }
        self.store.create_browser_session(user, now).await.unwrap()
    }

    async fn json(&self, method: &str, path: &str, token: &str, body: Option<Value>) -> Response {
        let url = format!("http://server.test/api/v1{path}");
        let request = match method {
            "GET" => TestClient::get(url),
            "POST" => TestClient::post(url),
            "PATCH" => TestClient::patch(url),
            "PUT" => TestClient::put(url),
            "DELETE" => TestClient::delete(url),
            other => panic!("unsupported test method {other}"),
        }
        .add_header("Authorization", format!("Bearer {token}"), true);
        let request = if let Some(body) = body {
            request.json(&body)
        } else {
            request
        };
        request.send(&self.service).await
    }

    async fn team(&self, actor: &ControlUser, slug: &str) -> String {
        self.store
            .create_tenant(actor, slug, slug, TenantQuota::default(), now_ms().unwrap())
            .await
            .unwrap()
            .tenant_id
            .to_string()
    }

    async fn close(self) {
        self.shutdown.send_replace(true);
        self.state.edge.shutdown().await;
    }
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "Verify real account pagination and role changes against existing browser credentials."
)]
async fn account_directory_is_paged_and_roles_do_not_grant_each_other_privileges() {
    let fixture = Fixture::new().await;
    let administrator = fixture.account("administrator", PlatformRole::Admin).await;
    let operator = fixture.account("operator", PlatformRole::Operator).await;
    let auditor = fixture.account("auditor", PlatformRole::Auditor).await;
    let user = fixture.account("ordinary", PlatformRole::User).await;
    for account in [&fixture.owner, &administrator, &auditor] {
        let response = fixture
            .json("GET", "/admin/accounts", &account.access_token, None)
            .await;
        assert_eq!(response.status_code, Some(StatusCode::OK));
        assert_eq!(response.headers().get("cache-control").unwrap(), "no-store");
    }
    for account in [&operator, &user] {
        assert_eq!(
            fixture
                .json("GET", "/admin/accounts", &account.access_token, None)
                .await
                .status_code,
            Some(StatusCode::FORBIDDEN),
        );
    }
    let mut cursor = None;
    let mut found = BTreeSet::new();
    loop {
        let path = cursor.as_ref().map_or_else(
            || "/admin/accounts?limit=2".to_owned(),
            |cursor| format!("/admin/accounts?limit=2&cursor={cursor}"),
        );
        let mut response = fixture
            .json("GET", &path, &auditor.access_token, None)
            .await;
        assert_eq!(response.status_code, Some(StatusCode::OK));
        let page: Value = response.take_json().await.unwrap();
        let accounts = page["accounts"].as_array().unwrap();
        assert!(accounts.len() <= 2);
        for account in accounts {
            assert!(found.insert(account["user_id"].as_str().unwrap().to_owned()));
            assert!(account.get("password_hash").is_none());
            assert!(account.get("access_token").is_none());
        }
        cursor = page["next_cursor"].as_str().map(str::to_owned);
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(found.len(), 5);
    let mut response = fixture
        .json(
            "GET",
            "/admin/accounts?query=AUDITOR&role=auditor",
            &auditor.access_token,
            None,
        )
        .await;
    let filtered: Value = response.take_json().await.unwrap();
    assert_eq!(filtered["accounts"].as_array().unwrap().len(), 1);
    assert_eq!(
        filtered["accounts"][0]["user_id"],
        auditor.session.user.user_id.as_str()
    );
    assert_eq!(
        fixture
            .json(
                "GET",
                "/admin/accounts?limit=101",
                &fixture.owner.access_token,
                None
            )
            .await
            .status_code,
        Some(StatusCode::BAD_REQUEST)
    );

    let role_path = format!("/admin/accounts/{}/role", user.session.user.user_id);
    for account in [&administrator, &operator, &auditor, &user] {
        assert_eq!(
            fixture
                .json(
                    "PATCH",
                    &role_path,
                    &account.access_token,
                    Some(json!({"role": "admin", "role_revision": 1}))
                )
                .await
                .status_code,
            Some(StatusCode::FORBIDDEN),
        );
    }
    let mut promoted = fixture
        .json(
            "PATCH",
            &role_path,
            &fixture.owner.access_token,
            Some(json!({"role": "operator", "role_revision": 1})),
        )
        .await;
    assert_eq!(promoted.status_code, Some(StatusCode::OK));
    let promoted: Value = promoted.take_json().await.unwrap();
    assert_eq!(promoted["platform_role"], "operator");
    assert_eq!(
        fixture
            .json(
                "PATCH",
                &role_path,
                &fixture.owner.access_token,
                Some(json!({"role":"admin", "role_revision": 1}))
            )
            .await
            .status_code,
        Some(StatusCode::CONFLICT)
    );
    let mut session = fixture
        .json("GET", "/auth/session", &user.access_token, None)
        .await;
    let session: Value = session.take_json().await.unwrap();
    assert_eq!(session["platform_role"], "operator");
    assert_eq!(
        fixture
            .json("GET", "/admin/workers", &user.access_token, None)
            .await
            .status_code,
        Some(StatusCode::OK)
    );
    let revision = promoted["role_revision"].as_u64().unwrap();
    assert_eq!(
        fixture
            .json(
                "PATCH",
                &role_path,
                &fixture.owner.access_token,
                Some(json!({"role":"user", "role_revision": revision}))
            )
            .await
            .status_code,
        Some(StatusCode::OK)
    );
    assert_eq!(
        fixture
            .json("GET", "/admin/workers", &user.access_token, None)
            .await
            .status_code,
        Some(StatusCode::FORBIDDEN)
    );
    fixture.close().await;
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "Verify Worker and maintenance privileges through the complete role and instance mode lifecycle."
)]
async fn execution_management_enforces_staff_duties_and_current_instance_mode() {
    let fixture = Fixture::new().await;
    let administrator = fixture.account("administrator", PlatformRole::Admin).await;
    let operator = fixture.account("operator", PlatformRole::Operator).await;
    let auditor = fixture.account("auditor", PlatformRole::Auditor).await;
    let user = fixture.account("ordinary", PlatformRole::User).await;
    for account in [&fixture.owner, &administrator, &operator, &auditor] {
        for path in ["/admin/workers", "/admin/execution", "/admin/instance"] {
            assert_eq!(
                fixture
                    .json("GET", path, &account.access_token, None)
                    .await
                    .status_code,
                Some(StatusCode::OK)
            );
        }
    }
    for path in ["/admin/workers", "/admin/execution", "/admin/instance"] {
        assert_eq!(
            fixture
                .json("GET", path, &user.access_token, None)
                .await
                .status_code,
            Some(StatusCode::FORBIDDEN)
        );
    }
    for account in [&auditor, &user] {
        assert_eq!(
            fixture
                .json(
                    "POST",
                    "/admin/workers",
                    &account.access_token,
                    Some(json!({"worker_id": "forbidden"}))
                )
                .await
                .status_code,
            Some(StatusCode::FORBIDDEN)
        );
        assert_eq!(
            fixture
                .json(
                    "PATCH",
                    "/admin/execution",
                    &account.access_token,
                    Some(json!({"claims_paused":true}))
                )
                .await
                .status_code,
            Some(StatusCode::FORBIDDEN)
        );
    }
    for (index, account) in [&administrator, &operator].into_iter().enumerate() {
        let worker_id = format!("staff-worker-{index}");
        let mut created = fixture
            .json(
                "POST",
                "/admin/workers",
                &account.access_token,
                Some(json!({"worker_id": worker_id})),
            )
            .await;
        assert_eq!(created.status_code, Some(StatusCode::CREATED));
        let grant: Value = created.take_json().await.unwrap();
        assert!(
            grant["token"]
                .as_str()
                .is_some_and(|token| !token.is_empty())
        );
        let mut listed = fixture
            .json("GET", "/admin/workers", &auditor.access_token, None)
            .await;
        let records: Value = listed.take_json().await.unwrap();
        assert!(
            records
                .as_array()
                .unwrap()
                .iter()
                .all(|record| record.get("token").is_none())
        );
        assert_eq!(
            fixture
                .json(
                    "DELETE",
                    &format!("/admin/workers/{worker_id}"),
                    &auditor.access_token,
                    None
                )
                .await
                .status_code,
            Some(StatusCode::FORBIDDEN)
        );
        assert_eq!(
            fixture
                .json(
                    "DELETE",
                    &format!("/admin/workers/{worker_id}"),
                    &account.access_token,
                    None
                )
                .await
                .status_code,
            Some(StatusCode::NO_CONTENT)
        );
    }
    let mut paused = fixture
        .json(
            "PATCH",
            "/admin/execution",
            &operator.access_token,
            Some(json!({"claims_paused":true})),
        )
        .await;
    assert_eq!(paused.status_code, Some(StatusCode::OK));
    assert_eq!(
        paused.take_json::<Value>().await.unwrap()["claims_paused"],
        true
    );
    for account in [&administrator, &operator, &auditor, &user] {
        assert_eq!(
            fixture
                .json(
                    "PATCH",
                    "/admin/instance",
                    &account.access_token,
                    Some(json!({"mode":"single_user", "revision":2}))
                )
                .await
                .status_code,
            Some(StatusCode::FORBIDDEN)
        );
    }
    assert_eq!(
        fixture
            .json(
                "PATCH",
                "/admin/instance",
                &fixture.owner.access_token,
                Some(json!({"mode":"single_user", "revision":2}))
            )
            .await
            .status_code,
        Some(StatusCode::OK)
    );
    for account in [&administrator, &operator, &auditor, &user] {
        for path in ["/auth/session", "/admin/workers"] {
            assert_eq!(
                fixture
                    .json("GET", path, &account.access_token, None)
                    .await
                    .status_code,
                Some(StatusCode::FORBIDDEN)
            );
        }
    }
    assert_eq!(
        fixture
            .json("GET", "/admin/workers", &fixture.owner.access_token, None)
            .await
            .status_code,
        Some(StatusCode::OK)
    );
    assert_eq!(
        fixture
            .json(
                "PATCH",
                "/admin/instance",
                &fixture.owner.access_token,
                Some(json!({"mode":"multi_user", "revision":3}))
            )
            .await
            .status_code,
        Some(StatusCode::OK)
    );
    assert_eq!(
        fixture
            .json("GET", "/admin/workers", &operator.access_token, None)
            .await
            .status_code,
        Some(StatusCode::OK)
    );
    fixture.close().await;
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "Verify native registration, personal defaults and explicit team joining through the HTTP API."
)]
async fn invitations_keep_personal_spaces_separate_from_team_membership() {
    let fixture = Fixture::new().await;
    let team_owner = fixture.account("team-owner", PlatformRole::User).await;
    let operator = fixture.account("operator", PlatformRole::Operator).await;
    let team_id = fixture.team(&team_owner.session.user, "project-team").await;
    let invite = json!({"role":"member"});
    assert_eq!(
        fixture
            .json(
                "POST",
                "/admin/invitations",
                &team_owner.access_token,
                Some(invite.clone())
            )
            .await
            .status_code,
        Some(StatusCode::FORBIDDEN)
    );
    assert_eq!(
        fixture
            .json(
                "POST",
                "/admin/invitations",
                &operator.access_token,
                Some(invite.clone())
            )
            .await
            .status_code,
        Some(StatusCode::FORBIDDEN)
    );
    let mut invitation = fixture
        .json(
            "POST",
            "/admin/invitations",
            &fixture.owner.access_token,
            Some(invite),
        )
        .await;
    assert_eq!(invitation.status_code, Some(StatusCode::CREATED));
    let invitation: Value = invitation.take_json().await.unwrap();
    assert!(invitation["tenant_id"].is_null());
    let mut accepted = TestClient::post("http://server.test/api/v1/auth/invitations/accept")
        .json(&json!({"token":invitation["token"], "username":"new-member", "email":"new-member@example.test", "password":"member-test-password"}))
        .send(&fixture.service).await;
    assert_eq!(accepted.status_code, Some(StatusCode::OK));
    let accepted: Value = accepted.take_json().await.unwrap();
    let token = accepted["access_token"].as_str().unwrap();
    let personal = accepted["personal_tenant_id"].as_str().unwrap();
    assert_ne!(personal, fixture.owner.session.personal_tenant_id.as_str());
    assert_eq!(accepted["platform_role"], "user");
    assert!(accepted["instance"].get("default_tenant_id").is_none());
    let mut spaces = fixture.json("GET", "/tenants", token, None).await;
    let spaces: Value = spaces.take_json().await.unwrap();
    assert_eq!(spaces["tenants"].as_array().unwrap().len(), 1);
    assert_eq!(spaces["tenants"][0]["kind"], "personal");

    let mut invitation = fixture
        .json(
            "POST",
            "/admin/invitations",
            &team_owner.access_token,
            Some(json!({"tenant_id":team_id,"role":"member"})),
        )
        .await;
    assert_eq!(invitation.status_code, Some(StatusCode::CREATED));
    let invitation: Value = invitation.take_json().await.unwrap();
    let mut joined = fixture
        .json(
            "POST",
            "/invitations/accept",
            token,
            Some(json!({"token":invitation["token"]})),
        )
        .await;
    assert_eq!(joined.status_code, Some(StatusCode::OK));
    assert_eq!(joined.take_json::<Value>().await.unwrap()["kind"], "team");
    let mut session = fixture.json("GET", "/auth/session", token, None).await;
    assert_eq!(
        session.take_json::<Value>().await.unwrap()["personal_tenant_id"],
        personal
    );
    let mut spaces = fixture.json("GET", "/tenants", token, None).await;
    assert_eq!(
        spaces.take_json::<Value>().await.unwrap()["tenants"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        fixture
            .json(
                "POST",
                "/invitations/accept",
                token,
                Some(json!({"token":invitation["token"]}))
            )
            .await
            .status_code,
        Some(StatusCode::FORBIDDEN)
    );
    let private_invitation = fixture
        .json(
            "POST",
            "/admin/invitations",
            &team_owner.access_token,
            Some(json!({"tenant_id":team_owner.session.personal_tenant_id, "role":"member"})),
        )
        .await;
    assert_eq!(private_invitation.status_code, Some(StatusCode::FORBIDDEN));
    fixture.close().await;
}

#[tokio::test]
async fn management_deep_links_render_and_old_management_routes_are_absent() {
    let fixture = Fixture::new().await;
    for path in [
        "/admin",
        "/admin/accounts",
        "/admin/instance",
        "/admin/workers",
        "/spaces/current",
    ] {
        let mut response = TestClient::get(format!("http://server.test{path}"))
            .send(&fixture.service)
            .await;
        assert_eq!(response.status_code, Some(StatusCode::OK));
        assert!(
            response
                .take_string()
                .await
                .unwrap()
                .contains("id=\"root\"")
        );
    }
    for path in ["/instance", "/instance/workers", "/instance/execution"] {
        assert_eq!(
            fixture
                .json("GET", path, &fixture.owner.access_token, None)
                .await
                .status_code,
            Some(StatusCode::NOT_FOUND)
        );
    }
    assert_eq!(
        TestClient::get("http://server.test/api/v1/admin/accounts")
            .send(&fixture.service)
            .await
            .status_code,
        Some(StatusCode::UNAUTHORIZED)
    );
    fixture.close().await;
}
