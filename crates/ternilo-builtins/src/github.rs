use linorun_core::{Activation, CleanupError, ComponentContext, ComponentDescriptor, effect};
use linorun_macros::component_descriptor;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{future::Future, pin::Pin, sync::Arc, time::Duration};
use ternilo_kernel::{
    HarnessPlugin, PluginFactory, PluginManifest, RunEnvironment, RunEnvironmentClient, ToolEffect,
    ToolExecutionContext, ToolHandler, ToolRegistration, Tools,
};
use ternilo_protocol::{HarnessError, ToolOutput, ToolSpec};

mod releases;

pub const KIND: &str = "ternilo.tools.github";
component_descriptor! { static DESCRIPTOR: () { id: "ternilo/builtin-github-tools@1", requires: [Tools, RunEnvironment], provides: [], } }
#[derive(Clone, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct Config {
    #[serde(default = "default_base")]
    base_url: String,
    #[serde(default)]
    credential_ref: Option<String>,
}
fn default_base() -> String {
    "https://api.github.com".into()
}
pub fn factory() -> PluginFactory {
    crate::factory(PluginManifest { kind: KIND, requires: &["ternilo/tools@1", "ternilo/run-environment@1"], provides: &[] }, |value| {
        let config: Config = crate::parse_config(value)?;
        let url = reqwest::Url::parse(&config.base_url).map_err(|error| HarnessError::invalid(error.to_string()))?;
        if !matches!(url.scheme(), "http" | "https") || !url.username().is_empty() || url.password().is_some() || url.query().is_some() || url.fragment().is_some() || config.credential_ref.as_ref().is_some_and(|value| value.trim().is_empty() || value.chars().any(char::is_control)) {
            return Err(HarnessError::invalid("GitHub requires an HTTP(S) API URL without credentials, query or fragment, and a valid credential reference"));
        }
        let client = if std::env::var_os("TERNILO_OUTER_SANDBOX").as_deref() == Some(std::ffi::OsStr::new("1")) {
            None
        } else {
            Some(reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .redirect(reqwest::redirect::Policy::none())
                .user_agent("Ternilo repository tools")
                .build()
                .map_err(|error| HarnessError::composition(format!("build GitHub HTTP client: {error}")))?)
        };
        Ok(Arc::new(Plugin { config, client }))
    }).with_config_schema::<Config>()
}
struct Plugin {
    config: Config,
    client: Option<reqwest::Client>,
}
#[derive(Clone, Copy)]
enum Operation {
    List,
    Review,
    ListReleases,
    ReleaseStatus,
    Create,
    Merge,
}
impl HarnessPlugin for Plugin {
    fn descriptor(&self) -> &'static ComponentDescriptor {
        &DESCRIPTOR
    }
    fn activate(&self, context: ComponentContext) -> Activation {
        let tools = context
            .context()
            .service::<Tools>()
            .expect("GitHub declares tools");
        let environment = context
            .context()
            .service::<RunEnvironment>()
            .expect("GitHub declares environment");
        let config = self.config.clone();
        let client = self.client.clone();
        Activation::Once(Box::pin(async move {
            let mut registrations = Vec::new();
            for (operation, spec) in specifications() {
                registrations.push(
                    tools
                        .register_tool(ToolRegistration {
                            spec,
                            effect: match operation {
                                Operation::List
                                | Operation::Review
                                | Operation::ListReleases
                                | Operation::ReleaseStatus => ToolEffect::ReadOnly,
                                Operation::Create | Operation::Merge => ToolEffect::Dangerous,
                            },
                            handler: Arc::new(GitHub {
                                config: config.clone(),
                                environment: environment.clone(),
                                client: client.clone(),
                                operation,
                            }),
                        })
                        .await
                        .map_err(|error| {
                            linorun_core::ActivationFailure::user(error.to_string())
                        })?,
                );
            }
            Ok(Some(effect::inverse(move || async move {
                for id in registrations {
                    tools
                        .unregister_tool(id)
                        .await
                        .map_err(|error| CleanupError::user(error.to_string()))?;
                }
                Ok(())
            })))
        }))
    }
}
fn specifications() -> Vec<(Operation, ToolSpec)> {
    let repository = json!({"type":"string","description":"GitHub owner/repository"});
    [
        (Operation::List, "github_list_prs", "List repository pull requests. Read-only; authentication is optional for public repositories.", json!({"repository":repository,"state":{"type":"string","enum":["open","closed","all"]},"page":{"type":"integer","minimum":1}}), vec!["repository"]),
        (Operation::Review, "github_review_pr", "Read the exact PR head, changed files, commit statuses and check runs before reviewing or merging. Pagination flags identify incomplete results; fetch remaining pages before claiming a complete review.", json!({"repository":repository,"number":{"type":"integer","minimum":1},"files_page":{"type":"integer","minimum":1},"checks_page":{"type":"integer","minimum":1},"statuses_page":{"type":"integer","minimum":1},"expected_head":{"type":"string","description":"Require the head SHA from an earlier review page"}}), vec!["repository","number"]),
        (Operation::ListReleases, "github_list_releases", "List releases visible to the configured GitHub identity, including drafts and prereleases when permitted. Read-only; follow has_more before claiming a complete list. Use github_release_status for the paginated asset inventory of a release.", json!({"repository":repository,"page":{"type":"integer","minimum":1}}), vec!["repository"]),
        (Operation::ReleaseStatus, "github_release_status", "Read release metadata and a page of published asset metadata. Select tag or release_id, not both; omit both for GitHub's latest non-draft, non-prerelease release. Use release_id from github_list_releases for drafts. Follow has_more_assets before claiming a complete inventory. This does not download or verify assets, resolve a tag to a commit, or prove installation works.", json!({"repository":repository,"tag":{"type":"string"},"release_id":{"type":"integer","minimum":1},"assets_page":{"type":"integer","minimum":1}}), vec!["repository"]),
        (Operation::Create, "github_create_pr", "Publish a pull request only within the user's authorization. Defaults to a draft. Include the problem, behavior and validation in the body. Requires a configured credential and normal tool approval.", json!({"repository":repository,"title":{"type":"string"},"body":{"type":"string"},"head":{"type":"string"},"base":{"type":"string"},"draft":{"type":"boolean","default":true}}), vec!["repository","title","body","head","base"]),
        (Operation::Merge, "github_merge_pr", "Merge an authorized, reviewed pull request only at its expected head SHA. Review changes and checks first. GitHub enforces repository permissions; this does not delete branches or add a bypass flag. Requires configured credentials and normal tool approval.", json!({"repository":repository,"number":{"type":"integer","minimum":1},"expected_head":{"type":"string"},"method":{"type":"string","enum":["merge","squash","rebase"]}}), vec!["repository","number","expected_head"]),
    ].into_iter().map(|(operation,name,description,properties,required)| (operation, ToolSpec { name: name.into(), description: description.into(), input_schema: json!({"type":"object","properties":properties,"required":required,"additionalProperties":false}) })).collect()
}
struct GitHub {
    config: Config,
    environment: RunEnvironmentClient,
    client: Option<reqwest::Client>,
    operation: Operation,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct List {
    repository: String,
    #[serde(default = "open_state")]
    state: String,
    #[serde(default = "first_page")]
    page: u32,
}
fn open_state() -> String {
    "open".into()
}
const fn first_page() -> u32 {
    1
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Review {
    repository: String,
    number: u64,
    #[serde(default = "first_page")]
    files_page: u32,
    #[serde(default = "first_page")]
    checks_page: u32,
    #[serde(default = "first_page")]
    statuses_page: u32,
    expected_head: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Create {
    repository: String,
    title: String,
    body: String,
    head: String,
    base: String,
    #[serde(default = "draft_default")]
    draft: bool,
}
const fn draft_default() -> bool {
    true
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Merge {
    repository: String,
    number: u64,
    expected_head: String,
    #[serde(default = "squash_default")]
    method: String,
}
fn squash_default() -> String {
    "squash".into()
}
struct Response {
    value: Value,
    more: bool,
}
fn decode<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, HarnessError> {
    serde_json::from_value(value).map_err(|error| HarnessError::invalid(error.to_string()))
}
fn endpoint(base: &str, repository: &str, path: &[&str]) -> Result<reqwest::Url, HarnessError> {
    let parts: Vec<_> = repository.split('/').collect();
    if parts.len() != 2
        || parts.iter().any(|part| {
            part.is_empty()
                || *part == "."
                || *part == ".."
                || !part
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
        })
    {
        return Err(HarnessError::invalid("repository must be owner/name"));
    }
    let mut url =
        reqwest::Url::parse(base).map_err(|error| HarnessError::invalid(error.to_string()))?;
    url.path_segments_mut()
        .map_err(|()| HarnessError::invalid("invalid GitHub API base URL"))?
        .pop_if_empty()
        .push("repos")
        .extend(parts)
        .extend(path);
    Ok(url)
}
fn pull(value: &Value) -> Value {
    json!({"number":value["number"],"url":value["html_url"],"title":value["title"],"body":value["body"],"state":value["state"],"draft":value["draft"],"mergeable":value["mergeable"],"mergeable_state":value["mergeable_state"],"head_sha":value["head"]["sha"],"head":value["head"]["ref"],"base_sha":value["base"]["sha"],"base":value["base"]["ref"]})
}
impl GitHub {
    async fn request(
        &self,
        method: reqwest::Method,
        url: reqwest::Url,
        body: Option<Value>,
        key: Option<&str>,
    ) -> Result<Response, HarnessError> {
        let mut request = self
            .client
            .as_ref()
            .ok_or_else(isolated_network)?
            .request(method, url)
            .header("accept", "application/vnd.github+json");
        if let Some(key) = key {
            request = request.bearer_auth(key);
        }
        if let Some(body) = body {
            request = request.json(&body);
        }
        let mut response = request
            .send()
            .await
            .map_err(|error| HarnessError::execution(format!("GitHub request: {error}")))?;
        let status = response.status();
        let more = response
            .headers()
            .get("link")
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.contains("rel=\"next\""));
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|error| HarnessError::execution(error.to_string()))?
        {
            if bytes.len() + chunk.len() > 2 * 1024 * 1024 {
                return Err(HarnessError::execution(
                    "GitHub response exceeds 2 MiB; inspect the repository locally",
                ));
            }
            bytes.extend_from_slice(&chunk);
        }
        let value: Value = serde_json::from_slice(&bytes)
            .map_err(|error| HarnessError::execution(format!("GitHub response: {error}")))?;
        if !status.is_success() {
            return Err(HarnessError::execution(format!(
                "GitHub HTTP {}: {}",
                status.as_u16(),
                value["message"].as_str().unwrap_or("request failed")
            )));
        }
        Ok(Response { value, more })
    }
    async fn review(&self, args: Review, key: Option<&str>) -> Result<Value, HarnessError> {
        use reqwest::Method;
        let base = &self.config.base_url;
        if args.number == 0
            || args.files_page == 0
            || args.checks_page == 0
            || args.statuses_page == 0
        {
            return Err(HarnessError::invalid(
                "PR number and pages must be positive",
            ));
        }
        let number = args.number.to_string();
        let summary = self
            .request(
                Method::GET,
                endpoint(base, &args.repository, &["pulls", &number])?,
                None,
                key,
            )
            .await?
            .value;
        let head = summary["head"]["sha"]
            .as_str()
            .ok_or_else(|| HarnessError::execution("GitHub PR is missing its head SHA"))?;
        if args
            .expected_head
            .as_deref()
            .is_some_and(|expected| expected != head)
        {
            return Err(HarnessError::conflict(
                "PR head changed; restart the review",
            ));
        }
        let mut files_url = endpoint(base, &args.repository, &["pulls", &number, "files"])?;
        files_url
            .query_pairs_mut()
            .append_pair("per_page", "30")
            .append_pair("page", &args.files_page.to_string());
        let files = self.request(Method::GET, files_url, None, key).await?;
        let mut checks_url = endpoint(base, &args.repository, &["commits", head, "check-runs"])?;
        checks_url
            .query_pairs_mut()
            .append_pair("per_page", "100")
            .append_pair("page", &args.checks_page.to_string());
        let checks = self.request(Method::GET, checks_url, None, key).await?;
        let mut statuses_url = endpoint(base, &args.repository, &["commits", head, "status"])?;
        statuses_url
            .query_pairs_mut()
            .append_pair("per_page", "100")
            .append_pair("page", &args.statuses_page.to_string());
        let statuses = self.request(Method::GET, statuses_url, None, key).await?;
        let latest = self
            .request(
                Method::GET,
                endpoint(base, &args.repository, &["pulls", &number])?,
                None,
                key,
            )
            .await?;
        if latest.value["head"]["sha"].as_str() != Some(head) {
            return Err(HarnessError::conflict(
                "PR head changed during review; read the new head before continuing",
            ));
        }
        Ok(
            json!({"pull":pull(&summary),"files":files.value,"files_page":args.files_page,"has_more_files":files.more,"checks":checks.value,"checks_page":args.checks_page,"has_more_checks":checks.more,"statuses":statuses.value,"statuses_page":args.statuses_page,"has_more_statuses":statuses.more}),
        )
    }

    async fn run(&self, arguments: Value, key: Option<&str>) -> Result<Value, HarnessError> {
        use reqwest::Method;
        let base = &self.config.base_url;
        match self.operation {
            Operation::List => {
                let args: List = decode(arguments)?;
                if !matches!(args.state.as_str(), "open" | "closed" | "all") || args.page == 0 {
                    return Err(HarnessError::invalid("invalid PR state or page"));
                }
                let mut url = endpoint(base, &args.repository, &["pulls"])?;
                url.query_pairs_mut()
                    .append_pair("state", &args.state)
                    .append_pair("per_page", "30")
                    .append_pair("page", &args.page.to_string());
                let response = self.request(Method::GET, url, None, key).await?;
                Ok(
                    json!({"pulls":response.value.as_array().ok_or_else(|| HarnessError::execution("GitHub PR list is not an array"))?.iter().map(pull).collect::<Vec<_>>(),"has_more":response.more,"page":args.page}),
                )
            }
            Operation::Review => self.review(decode(arguments)?, key).await,
            Operation::ListReleases => self.list_releases(decode(arguments)?, key).await,
            Operation::ReleaseStatus => self.release_status(decode(arguments)?, key).await,
            Operation::Create => {
                let args: Create = decode(arguments)?;
                if args.title.trim().is_empty()
                    || args.title.chars().count() > 256
                    || args.body.len() > 65_536
                    || args.head.trim().is_empty()
                    || args.base.trim().is_empty()
                {
                    return Err(HarnessError::invalid(
                        "PR creation requires a title, head, base and a body up to 64 KiB",
                    ));
                }
                let response = self.request(Method::POST, endpoint(base, &args.repository, &["pulls"] )?, Some(json!({"title":args.title,"body":args.body,"head":args.head,"base":args.base,"draft":args.draft})), key).await?;
                Ok(pull(&response.value))
            }
            Operation::Merge => {
                let args: Merge = decode(arguments)?;
                if args.number == 0
                    || !matches!(args.expected_head.len(), 40 | 64)
                    || !args
                        .expected_head
                        .bytes()
                        .all(|byte| byte.is_ascii_hexdigit())
                    || !matches!(args.method.as_str(), "merge" | "squash" | "rebase")
                {
                    return Err(HarnessError::invalid(
                        "merge requires a positive PR number, exact head SHA and merge/squash/rebase method",
                    ));
                }
                let response = self
                    .request(
                        Method::PUT,
                        endpoint(
                            base,
                            &args.repository,
                            &["pulls", &args.number.to_string(), "merge"],
                        )?,
                        Some(json!({"sha":args.expected_head,"merge_method":args.method})),
                        key,
                    )
                    .await?;
                Ok(response.value)
            }
        }
    }
}
impl ToolHandler for GitHub {
    fn execute<'a>(
        &'a self,
        context: ToolExecutionContext,
        arguments: Value,
    ) -> Pin<Box<dyn Future<Output = Result<ToolOutput, HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            self.environment
                .check_run_authorization(context.run_id)
                .await?;
            if self.client.is_none() {
                return Err(isolated_network());
            }
            let key = if let Some(reference) = &self.config.credential_ref {
                Some(
                    self.environment
                        .resolve_secret(reference.clone())
                        .await?
                        .filter(|value| !value.trim().is_empty())
                        .ok_or_else(|| {
                            HarnessError::invalid("configured GitHub credential is unavailable")
                        })?,
                )
            } else {
                None
            };
            if matches!(self.operation, Operation::Create | Operation::Merge) && key.is_none() {
                return Err(HarnessError::invalid(
                    "GitHub publishing requires a configured credential reference",
                ));
            }
            let result = self.run(arguments, key.as_deref()).await?;
            let is_error = matches!(self.operation, Operation::Merge) && result["merged"] != true;
            Ok(ToolOutput {
                content: serde_json::to_string(&result)
                    .map_err(|error| HarnessError::execution(error.to_string()))?,
                is_error,
            })
        })
    }
}

fn isolated_network() -> HarnessError {
    HarnessError::policy(
        "GitHub tools need network access. The managed Worker sandbox is offline; use a connected computer for GitHub operations.",
    )
}
