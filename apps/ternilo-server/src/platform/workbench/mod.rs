mod agent_team;
mod cloud_adapter;
mod discovery;
mod edge_adapter;
mod files;
mod model_computers;
pub(crate) mod model_options;
mod placement;
mod session_queue;
mod sessions;
mod settings;
mod sharing;
mod types;
mod workspace;

pub(crate) use cloud_adapter::CloudAdapter;
pub(crate) use edge_adapter::{EdgeAdapter, authorize_edge_mutation};
pub(crate) use placement::{PlacementResolver, SessionTarget, SettingsTarget};
pub(crate) use workspace::load_state;

use salvo_core::prelude::Router;
mod workspace_browser;

#[expect(
    clippy::too_many_lines,
    reason = "Keep the shared workbench route tree visible in one place."
)]
pub(crate) fn router() -> Router {
    let sessions = Router::with_path("sessions")
        .get(sessions::list_sessions)
        .push(Router::with_path("archived").get(sessions::list_archived_sessions))
        .post(sessions::create_session)
        .push(
            Router::with_path("{session_id}")
                .patch(sessions::update_session)
                .delete(sessions::delete_session)
                .push(sharing::router())
                .push(Router::with_path("fork").post(sessions::fork_session))
                .push(Router::with_path("archive").post(sessions::archive_session))
                .push(Router::with_path("restore").post(sessions::restore_session))
                .push(Router::with_path("events").get(sessions::session_events))
                .push(Router::with_path("archive-events").get(sessions::archived_session_events))
                .push(Router::with_path("history").get(sessions::session_history))
                .push(Router::with_path("archive-history").get(sessions::archived_session_history))
                .push(Router::with_path("references").get(sessions::reference_candidates))
                .push(
                    Router::with_path("workspace")
                        .get(workspace_browser::info)
                        .post(workspace_browser::operate),
                )
                .push(Router::with_path("attachments/resolve").post(sessions::resolve_attachment))
                .push(Router::with_path("files/{file_id}/content").get(files::file_content))
                .push(Router::with_path("plugins").get(sessions::session_plugins))
                .push(Router::with_path("commands").get(sessions::session_commands))
                .push(Router::with_path("services").get(sessions::session_services))
                .push(
                    Router::with_path("services/{service_id}/start")
                        .post(sessions::start_session_service),
                )
                .push(
                    Router::with_path("services/{service_id}/stop")
                        .post(sessions::stop_session_service),
                )
                .push(Router::with_path("stats").get(sessions::session_stats))
                .push(Router::with_path("projection").get(sessions::session_projection))
                .push(Router::with_path("telemetry").get(sessions::session_telemetry))
                .push(Router::with_path("export").get(sessions::export_session))
                .push(Router::with_path("feedback").post(sessions::record_feedback))
                .push(
                    Router::with_path("commands/feedback").post(sessions::record_command_feedback),
                )
                .push(
                    Router::with_path("subagents/{subagent_id}")
                        .push(Router::with_path("followup").post(sessions::followup_subagent))
                        .push(Router::with_path("interrupt").post(sessions::interrupt_subagent)),
                )
                .push(agent_team::router())
                .push(session_queue::router())
                .push(Router::with_path("skills").get(sessions::session_skills))
                .push(Router::with_path("skills/{skill_name}/turns").post(sessions::run_skill_turn))
                .push(
                    Router::with_path("turns")
                        .post(sessions::run_turn)
                        .push(Router::with_path("{run_id}").delete(sessions::cancel_turn)),
                ),
        );
    Router::new()
        .push(Router::with_path("catalog").get(crate::platform::workbench_catalog))
        .push(
            Router::with_path("agent-presets")
                .get(settings::list_agent_presets)
                .post(settings::copy_agent_preset)
                .push(
                    Router::with_path("{preset_id}")
                        .get(settings::get_agent_preset)
                        .put(settings::update_agent_preset)
                        .delete(settings::delete_agent_preset)
                        .push(Router::with_path("default").put(settings::set_default_agent_preset)),
                ),
        )
        .push(
            Router::with_path("projects")
                .get(crate::platform::list_projects)
                .post(crate::platform::create_project)
                .push(
                    Router::with_path("{project_id}")
                        .patch(crate::platform::rename_project)
                        .delete(crate::platform::delete_project),
                ),
        )
        .push(
            Router::with_path("workspaces")
                .get(crate::platform::list_workspaces)
                .post(crate::platform::create_workspace)
                .push(
                    Router::with_path("{workspace_id}")
                        .get(crate::platform::get_workspace)
                        .patch(workspace::rename_workspace)
                        .delete(workspace::unregister_workspace)
                        .push(Router::with_path("location").get(workspace::workspace_location))
                        .push(sharing::router()),
                ),
        )
        .push(Router::with_path("execution-targets").get(workspace::execution_targets))
        .push(
            Router::with_path("executors/{executor_id}/directories")
                .get(workspace::node_directory_listing)
                .post(workspace::make_node_directory),
        )
        .push(Router::with_path("state").get(workspace::workbench_state))
        .push(Router::with_path("files").get(files::list_files))
        .push(Router::with_path("model-options").get(model_options::model_options))
        .push(
            Router::with_path("model-computers")
                .get(model_computers::list)
                .push(Router::with_path("{executor_id}/usage").get(model_computers::usage)),
        )
        .push(
            Router::with_path("default-model")
                .get(sessions::get_default_model)
                .put(sessions::set_default_model),
        )
        .push(
            Router::with_path("sidebar-ordering")
                .get(settings::get_sidebar_ordering)
                .put(settings::set_sidebar_ordering),
        )
        .push(
            Router::with_path("providers")
                .get(settings::list_providers)
                .post(settings::upsert_provider)
                .push(
                    Router::with_path("from-extension")
                        .post(settings::materialize_extension_provider),
                )
                .push(Router::with_path("discover").post(settings::discover_provider_models))
                .push(Router::with_path("{provider_id}").delete(settings::delete_provider)),
        )
        .push(
            Router::with_path("extensions")
                .get(crate::platform::workbench_plugins)
                .post(crate::platform::install_extension)
                .push(
                    Router::with_path("publishers")
                        .post(crate::platform::trust_extension_publisher)
                        .push(
                            Router::with_path("{key_id}/revoke")
                                .post(crate::platform::revoke_extension_publisher),
                        ),
                )
                .push(
                    Router::with_path("{package_id}/{version}")
                        .put(crate::platform::set_extension_state)
                        .delete(crate::platform::uninstall_extension)
                        .push(Router::with_path("revoke").post(crate::platform::revoke_extension)),
                ),
        )
        .push(
            Router::with_path("credentials")
                .get(settings::list_credentials)
                .post(settings::set_credential)
                .push(Router::with_path("{name}").delete(settings::delete_credential)),
        )
        .push(
            Router::with_path("credential-records")
                .post(settings::set_credential_record)
                .push(
                    Router::with_path("{scope}/{record_id}")
                        .delete(settings::delete_credential_record),
                ),
        )
        .push(
            Router::with_path("authorizations")
                .get(settings::authorization_snapshot)
                .push(Router::with_path("begin").post(settings::begin_authorization))
                .push(Router::with_path("cancel").post(settings::cancel_authorization)),
        )
        .push(
            Router::with_path("authorization-prompts/answer")
                .post(settings::answer_authorization_prompt),
        )
        .push(
            Router::with_path("questions")
                .get(sessions::pending_questions)
                .push(Router::with_path("{question_id}/answer").post(sessions::answer_question)),
        )
        .push(Router::with_path("session-search").get(workspace::search_sessions))
        .push(sessions)
}

#[cfg(test)]
mod tests {
    use salvo_core::{routing::PathState, test::TestClient};

    use super::*;

    #[tokio::test]
    #[expect(
        clippy::too_many_lines,
        reason = "Keep setup, protocol actions and assertions together for this integration scenario."
    )]
    async fn workbench_mutation_attachment_and_node_directory_routes_are_mounted() {
        let cases = vec![
            TestClient::get("http://local.test/api/v1/catalog?session_id=session").build(),
            TestClient::get("http://local.test/api/v1/execution-targets").build(),
            TestClient::get("http://local.test/api/v1/default-model?session_id=session").build(),
            TestClient::put("http://local.test/api/v1/default-model?session_id=session").build(),
            TestClient::get("http://local.test/api/v1/executors/home/directories?path=%2Ftmp")
                .build(),
            TestClient::post("http://local.test/api/v1/executors/home/directories").build(),
            TestClient::get("http://local.test/api/v1/sessions/session/queue").build(),
            TestClient::post("http://local.test/api/v1/sessions/session/queue").build(),
            TestClient::patch("http://local.test/api/v1/sessions/session/queue/submission").build(),
            TestClient::delete("http://local.test/api/v1/sessions/session/queue/submission")
                .build(),
            TestClient::post("http://local.test/api/v1/sessions/session/queue/submission/steer")
                .build(),
            TestClient::post("http://local.test/api/v1/sessions/session/attachments/resolve")
                .build(),
            TestClient::post("http://local.test/api/v1/sessions/session/commands/feedback").build(),
            TestClient::post(
                "http://local.test/api/v1/sessions/session/subagents/agent-1/followup",
            )
            .build(),
            TestClient::post(
                "http://local.test/api/v1/sessions/session/subagents/agent-1/interrupt",
            )
            .build(),
            TestClient::get("http://local.test/api/v1/sessions/session/team").build(),
            TestClient::post("http://local.test/api/v1/sessions/session/team/tasks").build(),
            TestClient::put("http://local.test/api/v1/sessions/session/team/tasks/task-1").build(),
            TestClient::delete(
                "http://local.test/api/v1/sessions/session/team/tasks/task-1?expected_revision=1",
            )
            .build(),
            TestClient::post("http://local.test/api/v1/sessions/session/team/messages").build(),
            TestClient::put(
                "http://local.test/api/v1/sessions/session/team/messages/message-1/read",
            )
            .build(),
            TestClient::get("http://local.test/api/v1/questions?session_id=session").build(),
            TestClient::post(
                "http://local.test/api/v1/questions/question/answer?session_id=session",
            )
            .build(),
            TestClient::get("http://local.test/api/v1/agent-presets?workspace_id=workspace")
                .build(),
            TestClient::post("http://local.test/api/v1/agent-presets?session_id=session").build(),
            TestClient::get(
                "http://local.test/api/v1/agent-presets/standard?workspace_id=workspace",
            )
            .build(),
            TestClient::put("http://local.test/api/v1/agent-presets/custom?session_id=session")
                .build(),
            TestClient::delete(
                "http://local.test/api/v1/agent-presets/custom?workspace_id=workspace",
            )
            .build(),
            TestClient::put(
                "http://local.test/api/v1/agent-presets/custom/default?session_id=session",
            )
            .build(),
            TestClient::get("http://local.test/api/v1/credentials?session_id=session").build(),
            TestClient::post("http://local.test/api/v1/credentials?session_id=session").build(),
            TestClient::get("http://local.test/api/v1/providers?session_id=session").build(),
            TestClient::post("http://local.test/api/v1/providers?workspace_id=workspace").build(),
            TestClient::post(
                "http://local.test/api/v1/providers/from-extension?workspace_id=workspace",
            )
            .build(),
            TestClient::delete(
                "http://local.test/api/v1/providers/user-provider?session_id=session",
            )
            .build(),
            TestClient::post("http://local.test/api/v1/providers/discover?workspace_id=workspace")
                .build(),
            TestClient::get("http://local.test/api/v1/sidebar-ordering").build(),
            TestClient::put("http://local.test/api/v1/sidebar-ordering").build(),
            TestClient::delete(
                "http://local.test/api/v1/credentials/MY_KEY?workspace_id=workspace",
            )
            .build(),
            TestClient::post("http://local.test/api/v1/credential-records?session_id=session")
                .build(),
            TestClient::delete(
                "http://local.test/api/v1/credential-records/plugin/account?workspace_id=workspace",
            )
            .build(),
            TestClient::get("http://local.test/api/v1/extensions?session_id=session").build(),
            TestClient::post("http://local.test/api/v1/extensions?workspace_id=workspace").build(),
            TestClient::post("http://local.test/api/v1/extensions/publishers?session_id=session")
                .build(),
            TestClient::post(
                "http://local.test/api/v1/extensions/publishers/publisher-key/revoke?workspace_id=workspace",
            )
            .build(),
            TestClient::put("http://local.test/api/v1/extensions/package/1.0.0?session_id=session")
                .build(),
            TestClient::post(
                "http://local.test/api/v1/extensions/package/1.0.0/revoke?session_id=session",
            )
            .build(),
            TestClient::delete(
                "http://local.test/api/v1/extensions/package/1.0.0?workspace_id=workspace",
            )
            .build(),
            TestClient::get(
                "http://local.test/api/v1/authorizations?surface_id=settings&session_id=session",
            )
            .build(),
            TestClient::post(
                "http://local.test/api/v1/authorizations/begin?workspace_id=workspace",
            )
            .build(),
            TestClient::post("http://local.test/api/v1/authorizations/cancel?session_id=session")
                .build(),
            TestClient::post(
                "http://local.test/api/v1/authorization-prompts/answer?workspace_id=workspace",
            )
            .build(),
        ].into_boxed_slice();
        for mut request in cases {
            let root = Router::with_path("api/v1").push(router());
            let mut path = PathState::from_owned_path(request.uri().path().to_owned());
            assert!(
                root.detect(&mut request, &mut path).await.is_some(),
                "{} {} must resolve to a Control workbench handler",
                request.method(),
                request.uri().path(),
            );
        }
    }

    #[tokio::test]
    async fn shared_web_route_surface_is_mounted_for_cloud_and_edge_sessions() {
        let cases = vec![
            TestClient::get("http://control.test/api/v1/catalog?session_id=session").build(),
            TestClient::get("http://control.test/api/v1/projects").build(),
            TestClient::post("http://control.test/api/v1/projects").build(),
            TestClient::get("http://control.test/api/v1/workspaces").build(),
            TestClient::post("http://control.test/api/v1/workspaces").build(),
            TestClient::get("http://control.test/api/v1/workspaces/workspace").build(),
            TestClient::get("http://control.test/api/v1/workspaces/workspace/location").build(),
            TestClient::patch("http://control.test/api/v1/workspaces/workspace").build(),
            TestClient::delete("http://control.test/api/v1/workspaces/workspace").build(),
            TestClient::get("http://control.test/api/v1/execution-targets").build(),
            TestClient::get("http://control.test/api/v1/executors/home/directories?path=%2Ftmp")
                .build(),
            TestClient::post("http://control.test/api/v1/executors/home/directories").build(),
            TestClient::get("http://control.test/api/v1/state").build(),
            TestClient::get("http://control.test/api/v1/sessions").build(),
            TestClient::post("http://control.test/api/v1/sessions").build(),
            TestClient::patch("http://control.test/api/v1/sessions/session").build(),
            TestClient::delete("http://control.test/api/v1/sessions/session").build(),
            TestClient::post("http://control.test/api/v1/sessions/session/fork").build(),
            TestClient::post("http://control.test/api/v1/sessions/session/archive").build(),
            TestClient::get("http://control.test/api/v1/sessions/session/events").build(),
            TestClient::get(
                "http://control.test/api/v1/sessions/session/references?directory=%2Ftmp&query=a",
            )
            .build(),
            TestClient::get("http://control.test/api/v1/sessions/session/plugins").build(),
            TestClient::get("http://control.test/api/v1/sessions/session/commands").build(),
            TestClient::post("http://control.test/api/v1/sessions/session/attachments/resolve")
                .build(),
            TestClient::get("http://control.test/api/v1/sessions/session/projection").build(),
            TestClient::get("http://control.test/api/v1/sessions/session/telemetry").build(),
            TestClient::get("http://control.test/api/v1/sessions/session/stats").build(),
            TestClient::get("http://control.test/api/v1/sessions/session/export").build(),
            TestClient::post("http://control.test/api/v1/sessions/session/feedback").build(),
            TestClient::post("http://control.test/api/v1/sessions/session/commands/feedback")
                .build(),
            TestClient::post(
                "http://control.test/api/v1/sessions/session/subagents/agent/followup",
            )
            .build(),
            TestClient::post(
                "http://control.test/api/v1/sessions/session/subagents/agent/interrupt",
            )
            .build(),
            TestClient::get("http://control.test/api/v1/sessions/session/team").build(),
            TestClient::post("http://control.test/api/v1/sessions/session/team/tasks").build(),
            TestClient::put("http://control.test/api/v1/sessions/session/team/tasks/task").build(),
            TestClient::delete(
                "http://control.test/api/v1/sessions/session/team/tasks/task?expected_revision=1",
            )
            .build(),
            TestClient::post("http://control.test/api/v1/sessions/session/team/messages").build(),
            TestClient::put(
                "http://control.test/api/v1/sessions/session/team/messages/message/read",
            )
            .build(),
            TestClient::get("http://control.test/api/v1/sessions/session/queue").build(),
            TestClient::post("http://control.test/api/v1/sessions/session/queue").build(),
            TestClient::patch("http://control.test/api/v1/sessions/session/queue/submission")
                .build(),
            TestClient::delete("http://control.test/api/v1/sessions/session/queue/submission")
                .build(),
            TestClient::post("http://control.test/api/v1/sessions/session/queue/submission/steer")
                .build(),
            TestClient::get("http://control.test/api/v1/sessions/session/skills").build(),
            TestClient::post("http://control.test/api/v1/sessions/session/skills/skill/turns")
                .build(),
            TestClient::post("http://control.test/api/v1/sessions/session/turns").build(),
            TestClient::delete("http://control.test/api/v1/sessions/session/turns/run").build(),
            TestClient::get("http://control.test/api/v1/session-search?query=test").build(),
            TestClient::get("http://control.test/api/v1/questions?session_id=session").build(),
            TestClient::post(
                "http://control.test/api/v1/questions/question/answer?session_id=session",
            )
            .build(),
        ]
        .into_boxed_slice();
        for mut request in cases {
            let root = Router::with_path("api/v1").push(router());
            let mut path = PathState::from_owned_path(request.uri().path().to_owned());
            assert!(
                root.detect(&mut request, &mut path).await.is_some(),
                "{} {} must resolve to a Control workbench handler",
                request.method(),
                request.uri().path(),
            );
        }
    }

    #[tokio::test]
    async fn legacy_event_delta_http_route_is_not_mounted() {
        let mut request =
            TestClient::get("http://local.test/api/v1/sessions/session/event-delta").build();
        let root = Router::with_path("api/v1").push(router());
        let mut path = PathState::from_owned_path(request.uri().path().to_owned());
        assert!(root.detect(&mut request, &mut path).await.is_none());
    }
}
