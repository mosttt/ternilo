use super::*;

async fn body(mut response: Response, expected: StatusCode) -> Value {
    let status = response.status_code;
    let value: Value = response.take_json().await.unwrap();
    assert_eq!(status, Some(expected), "{value}");
    value
}

async fn public_post(fixture: &Fixture, path: &str, mut value: Value) -> Response {
    if path == "login" {
        value.as_object_mut().unwrap().remove("email");
    }
    TestClient::post(format!("http://server.test/api/v1/auth/{path}"))
        .json(&value)
        .send(&fixture.service)
        .await
}

async fn set_policy(fixture: &Fixture, mode: &str, approval: bool, revision: u64) {
    body(
        fixture
            .json(
                "PATCH",
                "/admin/registration",
                &fixture.owner.access_token,
                Some(json!({"mode":mode,"require_approval":approval,"revision":revision})),
            )
            .await,
        StatusCode::OK,
    )
    .await;
}

fn registration(username: &str) -> Value {
    json!({"email":format!("{}@example.test", username.trim().to_ascii_lowercase()),"username":username,"password":"registration-test-password"})
}

#[tokio::test]
async fn registration_policy_has_current_authorization_and_public_discovery() {
    let fixture = Fixture::new().await;
    let administrator = fixture.account("administrator", PlatformRole::Admin).await;
    let auditor = fixture.account("auditor", PlatformRole::Auditor).await;
    let operator = fixture.account("operator", PlatformRole::Operator).await;
    let user = fixture.account("ordinary", PlatformRole::User).await;
    for account in [&fixture.owner, &administrator, &auditor] {
        let settings = body(
            fixture
                .json("GET", "/admin/registration", &account.access_token, None)
                .await,
            StatusCode::OK,
        )
        .await;
        assert_eq!(
            settings,
            json!({"mode":"invite","require_approval":false,"revision":1})
        );
    }
    for account in [&operator, &user] {
        assert_eq!(
            fixture
                .json("GET", "/admin/registration", &account.access_token, None)
                .await
                .status_code,
            Some(StatusCode::FORBIDDEN)
        );
    }
    let update = json!({"mode":"open","require_approval":true,"revision":1});
    for account in [&auditor, &operator, &user] {
        assert_eq!(
            fixture
                .json(
                    "PATCH",
                    "/admin/registration",
                    &account.access_token,
                    Some(update.clone())
                )
                .await
                .status_code,
            Some(StatusCode::FORBIDDEN)
        );
    }
    let saved = body(
        fixture
            .json(
                "PATCH",
                "/admin/registration",
                &administrator.access_token,
                Some(update.clone()),
            )
            .await,
        StatusCode::OK,
    )
    .await;
    assert_eq!(saved["revision"], 2);
    assert_eq!(
        fixture
            .json(
                "PATCH",
                "/admin/registration",
                &administrator.access_token,
                Some(update)
            )
            .await
            .status_code,
        Some(StatusCode::CONFLICT)
    );
    assert_eq!(
        fixture
            .json(
                "PATCH",
                "/admin/registration",
                &administrator.access_token,
                Some(json!({"mode":"invite","require_approval":true,"revision":2}))
            )
            .await
            .status_code,
        Some(StatusCode::BAD_REQUEST)
    );
    let config = TestClient::get("http://server.test/auth/config")
        .send(&fixture.service)
        .await;
    assert_eq!(config.headers().get("cache-control").unwrap(), "no-store");
    assert_eq!(body(config, StatusCode::OK).await["registration"], saved);
    fixture.close().await;
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "Verify policy changes against the same unconsumed invitation and account lifecycle."
)]
async fn open_registration_and_account_invitation_are_exclusive() {
    let fixture = Fixture::new().await;
    assert_eq!(
        public_post(&fixture, "register", registration("closed"))
            .await
            .status_code,
        Some(StatusCode::FORBIDDEN)
    );
    let invitation = body(
        fixture
            .json(
                "POST",
                "/admin/invitations",
                &fixture.owner.access_token,
                Some(json!({"role":"member"})),
            )
            .await,
        StatusCode::CREATED,
    )
    .await;
    set_policy(&fixture, "open", false, 1).await;
    assert_eq!(
        fixture
            .json(
                "POST",
                "/admin/invitations",
                &fixture.owner.access_token,
                Some(json!({"role":"member"}))
            )
            .await
            .status_code,
        Some(StatusCode::FORBIDDEN)
    );
    let mut invited = registration("invited");
    invited["token"] = invitation["token"].clone();
    assert_eq!(
        public_post(&fixture, "invitations/accept", invited.clone())
            .await
            .status_code,
        Some(StatusCode::FORBIDDEN)
    );
    let registered = body(
        public_post(&fixture, "register", registration("open-member")).await,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(registered["status"], "active");
    assert_eq!(
        registered["session"]["user"]["user_id"],
        registered["user_id"]
    );
    assert_eq!(registered["session"]["platform_role"], "user");
    let token = registered["session"]["access_token"].as_str().unwrap();
    let session = body(
        fixture.json("GET", "/auth/session", token, None).await,
        StatusCode::OK,
    )
    .await;
    assert_eq!(session["user"]["user_id"], registered["user_id"]);
    set_policy(&fixture, "invite", false, 2).await;
    let accepted = body(
        public_post(&fixture, "invitations/accept", invited).await,
        StatusCode::OK,
    )
    .await;
    assert!(
        accepted["access_token"]
            .as_str()
            .unwrap()
            .starts_with("ter_a_")
    );
    assert_eq!(
        body(
            fixture
                .json(
                    "GET",
                    "/admin/accounts?query=invited",
                    &fixture.owner.access_token,
                    None
                )
                .await,
            StatusCode::OK
        )
        .await["accounts"][0]["status"],
        "active"
    );
    set_policy(&fixture, "open", false, 3).await;
    fixture
        .store
        .set_instance_mode(
            &fixture.owner.session.user,
            InstanceMode::SingleUser,
            2,
            now_ms().unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        public_post(&fixture, "register", registration("single-user"))
            .await
            .status_code,
        Some(StatusCode::FORBIDDEN)
    );
    fixture.close().await;
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "Keep approval, rejection, credential denial and stable identity in one HTTP lifecycle."
)]
async fn pending_registrations_need_an_admin_decision_before_login() {
    let fixture = Fixture::new().await;
    let administrator = fixture.account("administrator", PlatformRole::Admin).await;
    let auditor = fixture.account("auditor", PlatformRole::Auditor).await;
    set_policy(&fixture, "open", true, 1).await;
    let pending = body(
        public_post(&fixture, "register", registration("pending-member")).await,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(pending["status"], "pending");
    assert!(pending["session"].is_null());
    assert!(pending.get("access_token").is_none());
    let denied = body(
        public_post(&fixture, "login", registration("pending-member")).await,
        StatusCode::FORBIDDEN,
    )
    .await;
    assert_eq!(
        denied["error"]["message"],
        "account registration is pending approval"
    );
    assert_eq!(
        public_post(
            &fixture,
            "login",
            json!({"username":"pending-member","password":"wrong-password"})
        )
        .await
        .status_code,
        Some(StatusCode::UNAUTHORIZED)
    );
    let accounts = body(
        fixture
            .json(
                "GET",
                "/admin/accounts?status=pending&limit=1",
                &auditor.access_token,
                None,
            )
            .await,
        StatusCode::OK,
    )
    .await;
    assert_eq!(accounts["accounts"].as_array().unwrap().len(), 1);
    assert_eq!(accounts["accounts"][0]["user_id"], pending["user_id"]);
    let path = format!(
        "/admin/accounts/{}/review",
        pending["user_id"].as_str().unwrap()
    );
    let decision = json!({"decision":"approve","status_revision":1});
    assert_eq!(
        fixture
            .json("POST", &path, &auditor.access_token, Some(decision.clone()))
            .await
            .status_code,
        Some(StatusCode::FORBIDDEN)
    );
    let approved = body(
        fixture
            .json(
                "POST",
                &path,
                &administrator.access_token,
                Some(decision.clone()),
            )
            .await,
        StatusCode::OK,
    )
    .await;
    assert_eq!(approved["status"], "active");
    assert_eq!(approved["status_revision"], 2);
    assert_eq!(
        fixture
            .json("POST", &path, &administrator.access_token, Some(decision))
            .await
            .status_code,
        Some(StatusCode::CONFLICT)
    );
    let login = body(
        public_post(&fixture, "login", registration("pending-member")).await,
        StatusCode::OK,
    )
    .await;
    assert_eq!(login["user"]["user_id"], pending["user_id"]);
    let rejected = body(
        public_post(&fixture, "register", registration("rejected-member")).await,
        StatusCode::CREATED,
    )
    .await;
    let path = format!(
        "/admin/accounts/{}/review",
        rejected["user_id"].as_str().unwrap()
    );
    body(
        fixture
            .json(
                "POST",
                &path,
                &administrator.access_token,
                Some(json!({"decision":"reject","status_revision":1})),
            )
            .await,
        StatusCode::OK,
    )
    .await;
    let denied = body(
        public_post(&fixture, "login", registration("rejected-member")).await,
        StatusCode::FORBIDDEN,
    )
    .await;
    assert_eq!(
        denied["error"]["message"],
        "account registration was rejected"
    );
    fixture.close().await;
}

#[tokio::test]
async fn a_team_invitation_requires_an_account_and_remains_usable_after_rejected_signup() {
    let fixture = Fixture::new().await;
    let member = fixture.account("team-owner", PlatformRole::User).await;
    let guest = fixture.account("guest", PlatformRole::User).await;
    let team_id = fixture.team(&member.session.user, "team").await;
    let invitation = body(
        fixture
            .json(
                "POST",
                "/admin/invitations",
                &member.access_token,
                Some(json!({"tenant_id":team_id,"role":"member"})),
            )
            .await,
        StatusCode::CREATED,
    )
    .await;
    let mut signup = registration("team-token-signup");
    signup["token"] = invitation["token"].clone();
    assert_eq!(
        public_post(&fixture, "invitations/accept", signup)
            .await
            .status_code,
        Some(StatusCode::FORBIDDEN)
    );
    let accounts = body(
        fixture
            .json(
                "GET",
                "/admin/accounts?query=team-token-signup",
                &fixture.owner.access_token,
                None,
            )
            .await,
        StatusCode::OK,
    )
    .await;
    assert!(accounts["accounts"].as_array().unwrap().is_empty());
    set_policy(&fixture, "open", true, 1).await;
    let joined = body(
        fixture
            .json(
                "POST",
                "/invitations/accept",
                &guest.access_token,
                Some(json!({"token":invitation["token"]})),
            )
            .await,
        StatusCode::OK,
    )
    .await;
    assert_eq!(joined["tenant_id"], team_id);
    fixture.close().await;
}

#[tokio::test]
async fn registration_exposes_only_canonical_username_and_rejects_nickname_fields() {
    let fixture = Fixture::new().await;
    set_policy(&fixture, "open", false, 1).await;
    let mut with_nickname = registration("canonical-user");
    with_nickname["display_name"] = json!("A separate nickname");
    assert_eq!(
        public_post(&fixture, "register", with_nickname)
            .await
            .status_code,
        Some(StatusCode::BAD_REQUEST)
    );
    let registered = body(
        public_post(&fixture, "register", registration("Canonical-User")).await,
        StatusCode::CREATED,
    )
    .await;
    let user = &registered["session"]["user"];
    assert_eq!(user["username"], "canonical-user");
    assert_eq!(user["user_id"], registered["user_id"]);
    assert!(user.get("display_name").is_none());
    assert!(user.get("email").is_none());
    assert_eq!(user.as_object().unwrap().len(), 2);
    let login = body(
        public_post(&fixture, "login", registration("CANONICAL-USER")).await,
        StatusCode::OK,
    )
    .await;
    assert_eq!(&login["user"], user);
    let accounts = body(
        fixture
            .json(
                "GET",
                "/admin/accounts?query=canonical-user",
                &fixture.owner.access_token,
                None,
            )
            .await,
        StatusCode::OK,
    )
    .await;
    let accounts = accounts["accounts"].as_array().unwrap();
    assert_eq!(accounts.len(), 1);
    assert_eq!(accounts[0]["user_id"], user["user_id"]);
    assert_eq!(accounts[0]["username"], "canonical-user");
    assert_eq!(accounts[0]["email"], "canonical-user@example.test");
    assert!(accounts[0].get("display_name").is_none());
    fixture.close().await;
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "Exercise account transitions and credential revocation through the actual HTTP boundary."
)]
async fn account_status_api_revokes_sessions_and_keeps_email_private_to_account_views() {
    let fixture = Fixture::new().await;
    let administrator = fixture.account("administrator", PlatformRole::Admin).await;
    let auditor = fixture.account("auditor", PlatformRole::Auditor).await;
    set_policy(&fixture, "open", false, 1).await;
    let signup = body(
        public_post(&fixture, "register", registration("lifecycle-member")).await,
        StatusCode::CREATED,
    )
    .await;
    let id = signup["user_id"].as_str().unwrap();
    let token = signup["session"]["access_token"].as_str().unwrap();
    assert_eq!(signup["session"]["email"], "lifecycle-member@example.test");
    assert!(signup["session"]["user"].get("email").is_none());
    let path = format!("/admin/accounts/{id}/status");
    for denied in [&auditor.access_token, token] {
        body(
            fixture
                .json(
                    "POST",
                    &path,
                    denied,
                    Some(json!({"action":"ban","status_revision":1})),
                )
                .await,
            StatusCode::FORBIDDEN,
        )
        .await;
    }
    let protected = format!(
        "/admin/accounts/{}/status",
        fixture.owner.session.user.user_id
    );
    body(
        fixture
            .json(
                "POST",
                &protected,
                &administrator.access_token,
                Some(json!({"action":"remove","status_revision":1})),
            )
            .await,
        StatusCode::FORBIDDEN,
    )
    .await;
    let self_path = format!(
        "/admin/accounts/{}/status",
        administrator.session.user.user_id
    );
    body(
        fixture
            .json(
                "POST",
                &self_path,
                &administrator.access_token,
                Some(json!({"action":"ban","status_revision":1})),
            )
            .await,
        StatusCode::FORBIDDEN,
    )
    .await;
    let banned = body(
        fixture
            .json(
                "POST",
                &path,
                &administrator.access_token,
                Some(json!({"action":"ban","status_revision":1})),
            )
            .await,
        StatusCode::OK,
    )
    .await;
    assert_eq!(banned["status"], "banned");
    body(
        fixture.json("GET", "/auth/session", token, None).await,
        StatusCode::UNAUTHORIZED,
    )
    .await;
    body(
        public_post(&fixture, "login", registration("lifecycle-member")).await,
        StatusCode::FORBIDDEN,
    )
    .await;
    body(
        fixture
            .json(
                "POST",
                &path,
                &administrator.access_token,
                Some(json!({"action":"unban","status_revision":1})),
            )
            .await,
        StatusCode::CONFLICT,
    )
    .await;
    let active = body(
        fixture
            .json(
                "POST",
                &path,
                &administrator.access_token,
                Some(json!({"action":"unban","status_revision":banned["status_revision"]})),
            )
            .await,
        StatusCode::OK,
    )
    .await;
    body(
        fixture.json("GET", "/auth/session", token, None).await,
        StatusCode::UNAUTHORIZED,
    )
    .await;
    let fresh = body(
        public_post(&fixture, "login", registration("lifecycle-member")).await,
        StatusCode::OK,
    )
    .await;
    assert_eq!(fresh["user"]["user_id"], id);
    let removed = body(
        fixture
            .json(
                "POST",
                &path,
                &administrator.access_token,
                Some(json!({"action":"remove","status_revision":active["status_revision"]})),
            )
            .await,
        StatusCode::OK,
    )
    .await;
    assert_eq!(removed["status"], "removed");
    body(
        fixture
            .json(
                "GET",
                "/auth/session",
                fresh["access_token"].as_str().unwrap(),
                None,
            )
            .await,
        StatusCode::UNAUTHORIZED,
    )
    .await;
    body(
        fixture
            .json(
                "POST",
                &path,
                &administrator.access_token,
                Some(json!({"action":"unban","status_revision":removed["status_revision"]})),
            )
            .await,
        StatusCode::CONFLICT,
    )
    .await;
    let listed = body(
        fixture
            .json(
                "GET",
                "/admin/accounts?status=removed&query=lifecycle-member%40example.test",
                &auditor.access_token,
                None,
            )
            .await,
        StatusCode::OK,
    )
    .await;
    assert_eq!(listed["accounts"][0]["user_id"], id);
    assert_eq!(
        listed["accounts"][0]["email"],
        "lifecycle-member@example.test"
    );
    fixture.close().await;
}
