use serde_json::json;
use ternilo_protocol::{PluginEntry, Profile, ProviderWebAccess, WebFetchMode, WebSearchMode};

/// Select a Provider's local tools without overriding an explicitly disabled session tool.
pub fn apply_provider_web_access(
    profile: &mut Profile,
    web: &ProviderWebAccess,
    overrides: &[PluginEntry],
) {
    let search_kinds = [
        crate::web_search::KIND,
        crate::web_search::BRAVE_KIND,
        crate::web_search::TAVILY_KIND,
        crate::web_search::DEEPSEEK_KIND,
    ];
    let fetch_kinds = [crate::web::KIND, crate::deepseek_web::KIND];
    if web.search.mode != WebSearchMode::Session {
        let disabled =
            capability_disabled(profile, overrides, &search_kinds, "provider-web-search");
        profile
            .plugins
            .retain(|entry| !search_kinds.contains(&entry.kind.as_str()) || !entry.enabled);
        let kind = match web.search.mode {
            WebSearchMode::Deepseek => Some(crate::web_search::DEEPSEEK_KIND),
            WebSearchMode::Brave => Some(crate::web_search::BRAVE_KIND),
            WebSearchMode::Tavily => Some(crate::web_search::TAVILY_KIND),
            WebSearchMode::Searxng => Some(crate::web_search::KIND),
            _ => None,
        };
        if let Some(kind) = kind.filter(|_| !disabled) {
            profile.plugins.push(PluginEntry { id: "provider-web-search".into(), kind: kind.into(), enabled: true,
                config: json!({"base_url":web.search.base_url,"timeout_ms":web.search.timeout_ms,"max_results":web.search.max_results,"provider_web_access":web}) });
        }
    }
    if web.fetch.mode != WebFetchMode::Session {
        let disabled = capability_disabled(profile, overrides, &fetch_kinds, "provider-web-fetch");
        let direct = profile
            .plugins
            .iter()
            .find(|entry| entry.kind == crate::web::KIND)
            .map(|entry| entry.config.clone());
        profile
            .plugins
            .retain(|entry| !fetch_kinds.contains(&entry.kind.as_str()) || !entry.enabled);
        let tool = match web.fetch.mode {
            WebFetchMode::Direct => Some((crate::web::KIND, direct.unwrap_or_else(|| json!({})))),
            WebFetchMode::Deepseek => Some((
                crate::deepseek_web::KIND,
                json!({"base_url":web.fetch.base_url.as_deref().unwrap_or("https://api.deepseek.com"),"timeout_ms":web.fetch.timeout_ms,"max_bytes":web.fetch.max_bytes,"provider_web_access":web}),
            )),
            _ => None,
        };
        if let Some((kind, config)) = tool.filter(|_| !disabled) {
            profile.plugins.push(PluginEntry {
                id: "provider-web-fetch".into(),
                kind: kind.into(),
                enabled: true,
                config,
            });
        }
    }
}

fn capability_disabled(
    profile: &Profile,
    overrides: &[PluginEntry],
    kinds: &[&str],
    managed_id: &str,
) -> bool {
    // A disabled alternative does not disable a capability that is already enabled.
    let enabled = profile
        .plugins
        .iter()
        .any(|entry| entry.enabled && kinds.contains(&entry.kind.as_str()));
    overrides.iter().any(|entry| {
        !entry.enabled
            && (entry.id == managed_id || !enabled && kinds.contains(&entry.kind.as_str()))
    })
}

#[must_use]
pub fn blocked_provider_web_tools(
    profile: &Profile,
    overrides: &[PluginEntry],
) -> Vec<ternilo_protocol::WebCapability> {
    let search = [
        crate::web_search::KIND,
        crate::web_search::BRAVE_KIND,
        crate::web_search::TAVILY_KIND,
        crate::web_search::DEEPSEEK_KIND,
    ];
    let fetch = [crate::web::KIND, crate::deepseek_web::KIND];
    let mut blocked = Vec::new();
    if capability_disabled(profile, overrides, &search, "provider-web-search") {
        blocked.push(ternilo_protocol::WebCapability::Search);
    }
    if capability_disabled(profile, overrides, &fetch, "provider-web-fetch") {
        blocked.push(ternilo_protocol::WebCapability::Fetch);
    }
    blocked
}

pub(crate) async fn host_web_restrictions(
    environment: &ternilo_kernel::RunEnvironmentClient,
) -> Result<Vec<ternilo_protocol::WebCapability>, ternilo_protocol::HarnessError> {
    let mut blocked = Vec::new();
    for (capability, name) in [
        (ternilo_protocol::WebCapability::Search, "web_search"),
        (ternilo_protocol::WebCapability::Fetch, "web_fetch"),
    ] {
        let call = ternilo_protocol::ToolCall {
            id: format!("hosted-policy-{name}"),
            name: name.into(),
            arguments: json!({}),
            presentation: None,
        };
        match environment
            .check_tool(call, ternilo_kernel::ToolEffect::ReadOnly)
            .await
        {
            Ok(ternilo_kernel::ToolAuthorization::Allow) => (),
            Ok(ternilo_kernel::ToolAuthorization::Ask { .. }) => blocked.push(capability),
            Err(error) if error.code == ternilo_protocol::ErrorCode::PolicyDenied => {
                blocked.push(capability);
            }
            Err(error) => return Err(error),
        }
    }
    Ok(blocked)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn entry(id: &str, kind: &str, enabled: bool) -> PluginEntry {
        PluginEntry {
            id: id.into(),
            kind: kind.into(),
            enabled,
            config: json!({}),
        }
    }
    #[test]
    fn provider_switch_replaces_one_capability_and_preserves_unrelated_plugins() {
        let original = Profile {
            plugins: vec![
                entry("files", "ternilo.tool.files", true),
                entry("fetch", crate::web::KIND, true),
                entry("search", crate::web_search::KIND, true),
            ],
        };
        let mut profile = original.clone();
        apply_provider_web_access(&mut profile, &ProviderWebAccess::default(), &[]);
        assert_eq!(profile, original);
        let mut web = ProviderWebAccess::default();
        web.search.mode = WebSearchMode::Deepseek;
        web.search.api_key_ref = Some("WEB_KEY".into());
        web.fetch.mode = WebFetchMode::Deepseek;
        web.fetch.api_key_ref = Some("WEB_KEY".into());
        apply_provider_web_access(&mut profile, &web, &[]);
        assert_eq!(profile.plugins.len(), 3);
        assert_eq!(profile.plugins[0], original.plugins[0]);
        assert_eq!(profile.plugins[1].kind, crate::web_search::DEEPSEEK_KIND);
        assert_eq!(profile.plugins[2].kind, crate::deepseek_web::KIND);
        web.search.mode = WebSearchMode::Hosted;
        web.fetch.mode = WebFetchMode::Disabled;
        apply_provider_web_access(&mut profile, &web, &[]);
        assert_eq!(profile.plugins, vec![original.plugins[0].clone()]);
    }
    #[test]
    fn provider_web_replaces_an_enabled_alternative_without_reviving_disabled_tools() {
        let overrides = vec![
            entry("web-fetch", crate::web::KIND, false),
            entry("existing-fetch", crate::deepseek_web::KIND, true),
        ];
        let mut profile = Profile {
            plugins: overrides.clone(),
        };
        assert!(blocked_provider_web_tools(&profile, &overrides).is_empty());
        let mut web = ProviderWebAccess::default();
        web.fetch.mode = WebFetchMode::Deepseek;
        apply_provider_web_access(&mut profile, &web, &overrides);
        assert!(
            profile
                .plugins
                .iter()
                .any(|item| item.id == "web-fetch" && !item.enabled)
        );
        assert_eq!(
            profile
                .plugins
                .iter()
                .filter(|item| item.enabled)
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            ["provider-web-fetch"]
        );
        let blocked = vec![entry(
            "provider-web-fetch",
            crate::deepseek_web::KIND,
            false,
        )];
        apply_provider_web_access(&mut profile, &web, &blocked);
        assert!(profile.plugins.iter().all(|item| !item.enabled));
    }

    #[test]
    fn provider_plugins_cannot_redirect_authorized_credentials() {
        let mut web = ProviderWebAccess::default();
        web.search.mode = WebSearchMode::Deepseek;
        web.search.api_key_ref = Some("WEB_KEY".into());
        let mut profile = Profile {
            plugins: Vec::new(),
        };
        apply_provider_web_access(&mut profile, &web, &[]);
        let mut config = profile.plugins[0].config.clone();
        crate::web_search::deepseek_factory()
            .build(config.clone())
            .unwrap();
        config["base_url"] = json!("https://different-service.example");
        assert!(crate::web_search::deepseek_factory().build(config).is_err());
    }
    #[test]
    fn explicit_session_disable_also_blocks_the_provider_replacement() {
        let blocked = vec![
            entry("search", crate::web_search::KIND, false),
            entry("fetch", crate::web::KIND, false),
        ];
        let mut profile = Profile {
            plugins: blocked.clone(),
        };
        let mut web = ProviderWebAccess::default();
        web.search.mode = WebSearchMode::Deepseek;
        web.fetch.mode = WebFetchMode::Direct;
        apply_provider_web_access(&mut profile, &web, &blocked);
        assert_eq!(
            profile.plugins, blocked,
            "disabled controls remain visible for explicit re-enabling"
        );
        assert_eq!(
            blocked_provider_web_tools(&profile, &blocked),
            vec![
                ternilo_protocol::WebCapability::Search,
                ternilo_protocol::WebCapability::Fetch
            ]
        );
    }
}

#[cfg(test)]
mod host_tests {
    use super::*;
    use std::{future::Future, pin::Pin, sync::Arc};
    use ternilo_kernel::{
        HarnessSession, HostEnvironment, HostPolicy, ModelGatewayProvider, ModelOutput,
        RunCancellation,
    };
    use ternilo_protocol::{HarnessError, ModelRequest, ModelResponse, RunLimits, RunModelBinding};

    struct PolicyGateway;
    impl ModelGatewayProvider for PolicyGateway {
        fn complete<'a>(
            &'a self,
            _: linorun_core::CallContext<()>,
            _: RunModelBinding,
            request: ModelRequest,
            _: Arc<dyn ModelOutput>,
            _: RunCancellation,
        ) -> Pin<Box<dyn Future<Output = Result<ModelResponse, HarnessError>> + Send + 'a>>
        {
            Box::pin(async move {
                assert!(
                    request
                        .blocked_web_tools
                        .contains(&ternilo_protocol::WebCapability::Search)
                );
                assert!(
                    !request
                        .blocked_web_tools
                        .contains(&ternilo_protocol::WebCapability::Fetch)
                );
                Ok(serde_json::from_value(json!({"provider":"fixture","model":"model","content":"done","tool_calls":[],"finish_reason":"stop","attempts":1,"replayed":false})).unwrap())
            })
        }
    }

    #[tokio::test]
    async fn provider_web_host_policy_reaches_the_model_gateway() {
        let mut catalog = crate::catalog().unwrap();
        catalog.register(crate::model_gateway_factory()).unwrap();
        let mut profile = crate::local_profile();
        let model = profile
            .plugins
            .iter_mut()
            .find(|entry| entry.id == "model")
            .unwrap();
        model.kind = crate::BROKERED_MODEL_KIND.into();
        model.config = json!({"snapshot":{"binding":{"source":"user_provider","tenant_id":"tenant","owner_user_id":"owner","provider_id":"fixture","model":"model"},"protocol":"deepseek-responses","defaults":{"context_window":4096,"max_output_tokens":128},"display_name":"Model","source_name":"Account"}});
        let mut policy = HostPolicy::local(RunLimits::default());
        policy.denied_tools.insert("web_search".into());
        let identity = serde_json::from_value(json!({"tenant_id":"local","user_id":"owner","session_id":"web-policy","agent_id":"default"})).unwrap();
        let host = HostEnvironment::memory(identity, None, policy)
            .with_model_gateway(Arc::new(PolicyGateway));
        let harness = HarnessSession::boot(&catalog, &profile, host)
            .await
            .unwrap();
        assert_eq!(
            harness
                .run(
                    ternilo_protocol::RunId::new("web-policy"),
                    "Follow the host web policy"
                )
                .await
                .unwrap()
                .answer,
            "done"
        );
        harness.shutdown().await.unwrap();
    }
}
