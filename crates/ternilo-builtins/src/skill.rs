use std::{
    collections::BTreeMap,
    ffi::OsStr,
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    sync::{Arc, Mutex},
    time::Duration,
};

use linorun_core::{
    Activation, CallContext, CleanupError, ComponentContext, ComponentDescriptor, FiberId, effect,
};
use linorun_macros::component_descriptor;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use ternilo_kernel::{
    HarnessPlugin, PluginFactory, PluginManifest, RunEnvironment, SkillCandidate, SkillProvider,
    SkillProviderObservation, SkillProviderRegistration, Skills, SkillsClient, SkillsProvider,
    ToolExecutionContext, ToolHandler, ToolRegistration, Tools,
};
use ternilo_protocol::{
    HarnessError, PreparedSkillInvocation, SkillCatalogSnapshot, SkillDefinition,
    SkillInvocationPolicy, SkillSummary, ToolOutput, ToolSpec,
};
use tokio_util::sync::CancellationToken;

use crate::{EmptyConfig, factory as make_factory, parse_config};

pub const REGISTRY_KIND: &str = "ternilo.skills.registry";
pub const FILESYSTEM_KIND: &str = "ternilo.skills.filesystem";
pub const TOOL_KIND: &str = "ternilo.tools.skills";

const MAX_SKILLS_PER_PROVIDER: usize = 2_000;
const MAX_SKILL_CONTENT_BYTES: usize = 2 * 1024 * 1024;

component_descriptor! {
    static REGISTRY_DESCRIPTOR: () {
        id: "ternilo/builtin-skill-registry@1",
        requires: [],
        provides: [Skills],
    }
}

component_descriptor! {
    static FILESYSTEM_DESCRIPTOR: () {
        id: "ternilo/builtin-filesystem-skills@1",
        requires: [Skills, RunEnvironment],
        provides: [],
    }
}

component_descriptor! {
    static TOOL_DESCRIPTOR: () {
        id: "ternilo/builtin-skill-tools@2",
        requires: [Tools, Skills],
        provides: [],
    }
}

pub fn registry_factory() -> PluginFactory {
    make_factory(
        PluginManifest {
            kind: REGISTRY_KIND,
            requires: &[],
            provides: &["ternilo/skills@1"],
        },
        |value| {
            let _: EmptyConfig = parse_config(value)?;
            Ok(Arc::new(SkillRegistryPlugin))
        },
    )
    .with_config_schema::<EmptyConfig>()
}

pub fn filesystem_factory() -> PluginFactory {
    make_factory(
        PluginManifest {
            kind: FILESYSTEM_KIND,
            requires: &["ternilo/skills@1", "ternilo/run-environment@1"],
            provides: &[],
        },
        |value| {
            let config: FilesystemSkillConfig = parse_config(value)?;
            config.validate()?;
            Ok(Arc::new(FilesystemSkillPlugin { config }))
        },
    )
    .with_config_schema::<FilesystemSkillConfig>()
}

pub fn tool_factory() -> PluginFactory {
    make_factory(
        PluginManifest {
            kind: TOOL_KIND,
            requires: &["ternilo/tools@1", "ternilo/skills@1"],
            provides: &[],
        },
        |value| {
            let _: EmptyConfig = parse_config(value)?;
            Ok(Arc::new(SkillToolsPlugin))
        },
    )
    .with_config_schema::<EmptyConfig>()
}

struct SkillRegistryPlugin;

impl HarnessPlugin for SkillRegistryPlugin {
    fn descriptor(&self) -> &'static ComponentDescriptor {
        &REGISTRY_DESCRIPTOR
    }

    fn activate(&self, context: ComponentContext) -> Activation {
        let route = context.context().clone();
        let scope = context.scope().clone();
        let provider: Arc<dyn SkillsProvider> = Arc::new(SkillRegistry::default());
        Activation::Once(Box::pin(async move {
            scope
                .provide::<Skills>(&route, provider)
                .await
                .map_err(|error| {
                    linorun_core::ActivationFailure::user(format!(
                        "provide skill registry: {error}"
                    ))
                })?;
            Ok(None)
        }))
    }
}

#[derive(Clone)]
struct RegisteredProvider {
    name: String,
    caller: Option<FiberId>,
    provider: Arc<dyn SkillProvider>,
}

#[derive(Clone)]
struct IndexedCandidate {
    candidate: SkillCandidate,
    provider_id: u64,
    provider_caller: Option<FiberId>,
    provider_registration: u64,
    local_order: usize,
    provider: Arc<dyn SkillProvider>,
}

struct CachedCatalog {
    snapshot: SkillCatalogSnapshot,
    winners: BTreeMap<String, IndexedCandidate>,
}

#[derive(Default)]
struct SkillRegistryState {
    next: u64,
    revision: u64,
    providers: BTreeMap<u64, RegisteredProvider>,
    names: BTreeMap<String, u64>,
    cache: Option<Arc<CachedCatalog>>,
}

#[derive(Default)]
struct SkillRegistry {
    state: Mutex<SkillRegistryState>,
}

impl SkillRegistry {
    fn lock(&self) -> Result<std::sync::MutexGuard<'_, SkillRegistryState>, HarnessError> {
        self.state
            .lock()
            .map_err(|_| HarnessError::execution("skill registry lock poisoned"))
    }

    fn bump(state: &mut SkillRegistryState) -> Result<(), HarnessError> {
        state.revision = state
            .revision
            .checked_add(1)
            .ok_or_else(|| HarnessError::execution("skill catalog revision exhausted"))?;
        state.cache = None;
        Ok(())
    }

    async fn collect(&self) -> Result<Arc<CachedCatalog>, HarnessError> {
        for attempt in 0..2 {
            let (revision, providers, cached) = {
                let state = self.lock()?;
                (
                    state.revision,
                    state
                        .providers
                        .iter()
                        .map(|(id, provider)| (*id, provider.clone()))
                        .collect::<Vec<_>>(),
                    state.cache.clone(),
                )
            };
            if let Some(cached) = cached.filter(|cached| cached.snapshot.revision == revision) {
                return Ok(cached);
            }

            let mut complete = true;
            let mut winners = BTreeMap::<String, IndexedCandidate>::new();
            for (provider_id, registered) in providers {
                let Ok(observation) = registered.provider.list().await else {
                    complete = false;
                    continue;
                };
                complete &= observation.complete;
                if observation.candidates.len() > MAX_SKILLS_PER_PROVIDER {
                    complete = false;
                }
                for (local_order, candidate) in observation
                    .candidates
                    .into_iter()
                    .take(MAX_SKILLS_PER_PROVIDER)
                    .enumerate()
                {
                    if validate_candidate(&candidate, &registered.name).is_err() {
                        complete = false;
                        continue;
                    }
                    let indexed = IndexedCandidate {
                        candidate,
                        provider_id,
                        provider_caller: registered.caller,
                        provider_registration: provider_id,
                        local_order,
                        provider: Arc::clone(&registered.provider),
                    };
                    let replace = winners
                        .get(&indexed.candidate.summary.name)
                        .is_none_or(|current| candidate_order(&indexed) < candidate_order(current));
                    if replace {
                        winners.insert(indexed.candidate.summary.name.clone(), indexed);
                    }
                }
            }
            let mut skills = winners
                .values()
                .map(|entry| entry.candidate.summary.clone())
                .collect::<Vec<_>>();
            skills.sort_by(|left, right| left.name.cmp(&right.name));
            let catalog = Arc::new(CachedCatalog {
                snapshot: SkillCatalogSnapshot {
                    revision,
                    complete,
                    skills,
                },
                winners,
            });

            let mut state = self.lock()?;
            if state.revision == revision {
                if complete {
                    state.cache = Some(Arc::clone(&catalog));
                }
                return Ok(catalog);
            }
            if attempt == 1 {
                let mut snapshot = catalog.snapshot.clone();
                snapshot.complete = false;
                snapshot.revision = state.revision;
                return Ok(Arc::new(CachedCatalog {
                    snapshot,
                    winners: catalog.winners.clone(),
                }));
            }
        }
        unreachable!("bounded skill collection attempts return from the loop")
    }

    async fn load(&self, name: &str) -> Result<Option<SkillDefinition>, HarnessError> {
        validate_skill_name(name)?;
        let catalog = self.collect().await?;
        let Some(indexed) = catalog.winners.get(name).cloned() else {
            return Ok(None);
        };
        let Some(definition) = indexed
            .provider
            .load(indexed.candidate.locator.clone())
            .await?
        else {
            self.invalidate_provider(indexed.provider_id)?;
            return Ok(None);
        };
        validate_definition(&definition)?;
        if definition.summary != indexed.candidate.summary {
            self.invalidate_provider(indexed.provider_id)?;
            return Err(HarnessError::execution(format!(
                "skill {name:?} changed while it was being loaded; refresh the catalog"
            )));
        }
        Ok(Some(definition))
    }

    fn invalidate_provider(&self, registration: u64) -> Result<(), HarnessError> {
        let mut state = self.lock()?;
        if !state.providers.contains_key(&registration) {
            return Err(HarnessError::execution(format!(
                "unknown skill provider registration {registration}"
            )));
        }
        Self::bump(&mut state)
    }

    #[cfg(test)]
    fn register_inner(&self, registration: SkillProviderRegistration) -> Result<u64, HarnessError> {
        self.register_with_caller(None, registration)
    }

    fn register_with_caller(
        &self,
        caller: Option<FiberId>,
        registration: SkillProviderRegistration,
    ) -> Result<u64, HarnessError> {
        validate_provider_name(&registration.name)?;
        let mut state = self.lock()?;
        if state.names.contains_key(&registration.name) {
            return Err(HarnessError::composition(format!(
                "skill provider {:?} is already registered",
                registration.name
            )));
        }
        let id = state.next;
        state.next = state
            .next
            .checked_add(1)
            .ok_or_else(|| HarnessError::execution("skill provider id exhausted"))?;
        state.names.insert(registration.name.clone(), id);
        state.providers.insert(
            id,
            RegisteredProvider {
                name: registration.name,
                caller,
                provider: registration.provider,
            },
        );
        Self::bump(&mut state)?;
        Ok(id)
    }
}

fn candidate_order(candidate: &IndexedCandidate) -> (u32, Option<FiberId>, u64, usize) {
    (
        candidate.candidate.rank,
        candidate.provider_caller,
        candidate.provider_registration,
        candidate.local_order,
    )
}

impl SkillsProvider for SkillRegistry {
    fn register_provider<'a>(
        &'a self,
        context: CallContext<()>,
        registration: SkillProviderRegistration,
    ) -> Pin<Box<dyn Future<Output = Result<u64, HarnessError>> + Send + 'a>> {
        Box::pin(async move { self.register_with_caller(Some(context.caller), registration) })
    }

    fn unregister_provider<'a>(
        &'a self,
        _: CallContext<()>,
        registration: u64,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            let mut state = self.lock()?;
            let provider = state.providers.remove(&registration).ok_or_else(|| {
                HarnessError::execution(format!(
                    "unknown skill provider registration {registration}"
                ))
            })?;
            state.names.remove(&provider.name);
            Self::bump(&mut state)
        })
    }

    fn invalidate<'a>(
        &'a self,
        _: CallContext<()>,
        registration: u64,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>> {
        Box::pin(async move { self.invalidate_provider(registration) })
    }

    fn snapshot<'a>(
        &'a self,
        _: CallContext<()>,
    ) -> Pin<Box<dyn Future<Output = Result<SkillCatalogSnapshot, HarnessError>> + Send + 'a>> {
        Box::pin(async move { Ok(self.collect().await?.snapshot.clone()) })
    }

    fn get<'a>(
        &'a self,
        _: CallContext<()>,
        name: String,
    ) -> Pin<Box<dyn Future<Output = Result<Option<SkillDefinition>, HarnessError>> + Send + 'a>>
    {
        Box::pin(async move { self.load(&name).await })
    }
}

fn validate_provider_name(name: &str) -> Result<(), HarnessError> {
    if name.is_empty()
        || name.len() > 128
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
    {
        Err(HarnessError::invalid(
            "skill provider name must be 1 to 128 ASCII identifier characters",
        ))
    } else {
        Ok(())
    }
}

fn validate_skill_name(name: &str) -> Result<(), HarnessError> {
    if name.is_empty()
        || name.len() > 128
        || name.split('-').any(|segment| {
            segment.is_empty()
                || !segment
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        })
    {
        Err(HarnessError::invalid(
            "skill name must be a 1 to 128 byte lowercase kebab-case identifier",
        ))
    } else {
        Ok(())
    }
}

fn validate_summary(summary: &SkillSummary) -> Result<(), HarnessError> {
    validate_skill_name(&summary.name)?;
    if summary.description.trim().is_empty()
        || summary.description.chars().count() > 2_000
        || summary
            .when_to_use
            .as_ref()
            .is_some_and(|value| value.chars().count() > 4_000)
        || summary.source.trim().is_empty()
    {
        return Err(HarnessError::invalid(
            "skill summary contains an invalid description, routing hint, or source",
        ));
    }
    validate_provider_name(&summary.provider)
}

fn validate_candidate(candidate: &SkillCandidate, provider: &str) -> Result<(), HarnessError> {
    validate_summary(&candidate.summary)?;
    if candidate.summary.provider != provider
        || candidate.locator.is_empty()
        || candidate.locator.len() > 16_384
    {
        Err(HarnessError::invalid(
            "skill candidate provider or locator is invalid",
        ))
    } else {
        Ok(())
    }
}

fn validate_definition(definition: &SkillDefinition) -> Result<(), HarnessError> {
    validate_summary(&definition.summary)?;
    if definition.content.trim().is_empty()
        || definition.content.len() > MAX_SKILL_CONTENT_BYTES
        || definition
            .resource_base
            .as_ref()
            .is_some_and(|value| value.len() > 16_384)
    {
        Err(HarnessError::invalid(
            "skill definition contains invalid or oversized content",
        ))
    } else {
        Ok(())
    }
}

#[derive(Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct FilesystemSkillConfig {
    #[serde(default = "default_provider_name")]
    provider_name: String,
    #[serde(default = "default_true")]
    include_default_roots: bool,
    #[serde(default)]
    custom_skill_dirs: Vec<String>,
    #[serde(default)]
    ternilo_home: Option<String>,
    #[serde(default)]
    agents_home: Option<String>,
    #[serde(default = "default_true")]
    watch: bool,
    #[serde(default = "default_watch_interval_ms")]
    watch_interval_ms: u64,
}

impl FilesystemSkillConfig {
    fn validate(&self) -> Result<(), HarnessError> {
        validate_provider_name(&self.provider_name)?;
        if !(100..=60_000).contains(&self.watch_interval_ms) {
            return Err(HarnessError::composition(
                "filesystem skill watch_interval_ms must be between 100 and 60000",
            ));
        }
        for root in &self.custom_skill_dirs {
            if root.trim().is_empty() || !Path::new(root).is_absolute() {
                return Err(HarnessError::composition(
                    "filesystem custom_skill_dirs must contain absolute non-empty paths",
                ));
            }
        }
        Ok(())
    }
}

fn default_provider_name() -> String {
    "filesystem".to_owned()
}

const fn default_true() -> bool {
    true
}

const fn default_watch_interval_ms() -> u64 {
    750
}

struct FilesystemSkillPlugin {
    config: FilesystemSkillConfig,
}

impl HarnessPlugin for FilesystemSkillPlugin {
    fn descriptor(&self) -> &'static ComponentDescriptor {
        &FILESYSTEM_DESCRIPTOR
    }

    fn activate(&self, context: ComponentContext) -> Activation {
        let skills = context
            .context()
            .service::<Skills>()
            .expect("filesystem skills declares Skills");
        let environment = context
            .context()
            .service::<RunEnvironment>()
            .expect("filesystem skills declares RunEnvironment");
        let config = self.config.clone();
        Activation::Once(Box::pin(async move {
            let workspace = environment.workspace().await;
            let roots =
                resolve_skill_roots(workspace.as_ref().map(|value| value.path.as_str()), &config)
                    .await
                    .map_err(|error| linorun_core::ActivationFailure::user(error.to_string()))?;
            let provider = Arc::new(FilesystemSkillProvider {
                name: config.provider_name.clone(),
                roots,
            });
            let registration = skills
                .register_provider(SkillProviderRegistration {
                    name: config.provider_name,
                    provider: provider.clone(),
                })
                .await
                .map_err(|error| linorun_core::ActivationFailure::user(error.to_string()))?;
            let cancellation = CancellationToken::new();
            let watcher = config.watch.then(|| {
                tokio::spawn(watch_filesystem_skills(
                    skills.clone(),
                    registration,
                    provider,
                    Duration::from_millis(config.watch_interval_ms),
                    cancellation.clone(),
                ))
            });
            Ok(Some(effect::inverse(move || async move {
                cancellation.cancel();
                if let Some(watcher) = watcher {
                    watcher.await.map_err(|error| {
                        CleanupError::user(format!("join filesystem skill watcher: {error}"))
                    })?;
                }
                skills
                    .unregister_provider(registration)
                    .await
                    .map_err(|error| CleanupError::user(error.to_string()))
            })))
        }))
    }
}

#[derive(Clone)]
struct SkillRoot {
    path: PathBuf,
    source: &'static str,
    rank: u32,
    skip_system: bool,
}

struct FilesystemSkillProvider {
    name: String,
    roots: Vec<SkillRoot>,
}

impl FilesystemSkillProvider {
    async fn discover(&self) -> SkillProviderObservation {
        let mut candidates = Vec::new();
        let mut complete = true;
        for root in &self.roots {
            match discover_root(root, &self.name).await {
                Ok(mut found) => candidates.append(&mut found),
                Err(_) => complete = false,
            }
        }
        SkillProviderObservation {
            candidates,
            complete,
        }
    }

    async fn fingerprint(&self) -> Vec<u8> {
        let observation = self.discover().await;
        let rows = observation
            .candidates
            .iter()
            .map(|candidate| {
                json!({
                    "summary": candidate.summary,
                    "rank": candidate.rank,
                    "locator": candidate.locator,
                })
            })
            .collect::<Vec<_>>();
        Sha256::digest(
            serde_json::to_vec(&(observation.complete, rows))
                .expect("skill fingerprint is JSON serializable"),
        )
        .to_vec()
    }

    async fn load_locator(&self, locator: &str) -> Result<Option<SkillDefinition>, HarnessError> {
        let path = PathBuf::from(locator);
        let Some(root) = self.roots.iter().find(|root| path.starts_with(&root.path)) else {
            return Err(HarnessError::policy(
                "filesystem skill locator is outside registered roots",
            ));
        };
        let canonical_root = match tokio::fs::canonicalize(&root.path).await {
            Ok(path) => path,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(io_error("canonicalize skill root", &root.path, &error)),
        };
        let canonical_path = match tokio::fs::canonicalize(&path).await {
            Ok(path) => path,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(io_error("canonicalize skill file", &path, &error)),
        };
        if !canonical_path.starts_with(canonical_root) {
            return Err(HarnessError::policy(
                "filesystem skill symlink escapes its configured root",
            ));
        }
        let parsed = parse_skill_file(&canonical_path).await?;
        Ok(Some(SkillDefinition {
            summary: SkillSummary {
                name: parsed.name,
                description: parsed.description,
                when_to_use: parsed.when_to_use,
                invocation: parsed.invocation,
                source: root.source.to_owned(),
                provider: self.name.clone(),
            },
            content: parsed.content,
            resource_base: canonical_path
                .parent()
                .map(|value| value.to_string_lossy().into_owned()),
        }))
    }
}

impl SkillProvider for FilesystemSkillProvider {
    fn list<'a>(
        &'a self,
    ) -> Pin<Box<dyn Future<Output = Result<SkillProviderObservation, HarnessError>> + Send + 'a>>
    {
        Box::pin(async move { Ok(self.discover().await) })
    }

    fn load<'a>(
        &'a self,
        locator: String,
    ) -> Pin<Box<dyn Future<Output = Result<Option<SkillDefinition>, HarnessError>> + Send + 'a>>
    {
        Box::pin(async move { self.load_locator(&locator).await })
    }
}

async fn watch_filesystem_skills(
    skills: SkillsClient,
    registration: u64,
    provider: Arc<FilesystemSkillProvider>,
    interval: Duration,
    cancellation: CancellationToken,
) {
    let mut previous = provider.fingerprint().await;
    let mut timer = tokio::time::interval(interval);
    timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    timer.tick().await;
    loop {
        tokio::select! {
            () = cancellation.cancelled() => return,
            _ = timer.tick() => {
                let next = provider.fingerprint().await;
                if next != previous {
                    previous = next;
                    if skills.invalidate(registration).await.is_err() {
                        return;
                    }
                }
            }
        }
    }
}

async fn resolve_skill_roots(
    workspace: Option<&str>,
    config: &FilesystemSkillConfig,
) -> Result<Vec<SkillRoot>, HarnessError> {
    let mut roots = Vec::new();
    if config.include_default_roots
        && let Some(workspace) = workspace
    {
        let project = find_project_root(PathBuf::from(workspace)).await;
        roots.push(SkillRoot {
            path: project.join(".ternilo/skills"),
            source: "project-ternilo",
            rank: 100,
            skip_system: false,
        });
        roots.push(SkillRoot {
            path: project.join(".agents/skills"),
            source: "project-agents",
            rank: 200,
            skip_system: false,
        });
    }
    roots.extend(config.custom_skill_dirs.iter().map(|path| SkillRoot {
        path: PathBuf::from(path),
        source: "custom",
        rank: 300,
        skip_system: false,
    }));
    if config.include_default_roots {
        let home = user_home();
        let ternilo_home = config
            .ternilo_home
            .as_ref()
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("TERNILO_HOME").map(PathBuf::from))
            .or_else(|| home.as_ref().map(|path| path.join(".ternilo")));
        let agents_home = config
            .agents_home
            .as_ref()
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("TERNILO_AGENTS_HOME").map(PathBuf::from))
            .or_else(|| home.map(|path| path.join(".agents")));
        if let Some(path) = ternilo_home {
            roots.push(SkillRoot {
                path: path.join("skills"),
                source: "user-ternilo",
                rank: 400,
                skip_system: true,
            });
        }
        if let Some(path) = agents_home {
            roots.push(SkillRoot {
                path: path.join("skills"),
                source: "user-agents",
                rank: 500,
                skip_system: false,
            });
        }
    }
    Ok(roots)
}

async fn find_project_root(mut current: PathBuf) -> PathBuf {
    let fallback = current.clone();
    loop {
        if tokio::fs::metadata(current.join(".git")).await.is_ok() {
            return current;
        }
        if !current.pop() {
            return fallback;
        }
    }
}

fn user_home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

async fn discover_root(
    root: &SkillRoot,
    provider: &str,
) -> Result<Vec<SkillCandidate>, HarnessError> {
    let mut directory = match tokio::fs::read_dir(&root.path).await {
        Ok(directory) => directory,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(io_error("read skill root", &root.path, &error)),
    };
    let mut paths = Vec::new();
    while let Some(entry) = directory
        .next_entry()
        .await
        .map_err(|error| io_error("enumerate skill root", &root.path, &error))?
    {
        if paths.len() >= MAX_SKILLS_PER_PROVIDER {
            break;
        }
        let name = entry.file_name();
        if root.skip_system && name == OsStr::new(".system") {
            continue;
        }
        let Ok(file_type) = entry.file_type().await else {
            continue;
        };
        let path = entry.path();
        if file_type.is_dir() {
            let skill = path.join("SKILL.md");
            if tokio::fs::metadata(&skill)
                .await
                .is_ok_and(|metadata| metadata.is_file())
            {
                paths.push(skill);
            }
        } else if file_type.is_file()
            && path
                .extension()
                .and_then(OsStr::to_str)
                .is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
        {
            paths.push(path);
        }
    }
    paths.sort();
    let mut candidates = Vec::new();
    for path in paths {
        let Ok(parsed) = parse_skill_file(&path).await else {
            continue;
        };
        let summary = SkillSummary {
            name: parsed.name,
            description: parsed.description,
            when_to_use: parsed.when_to_use,
            invocation: parsed.invocation,
            source: root.source.to_owned(),
            provider: provider.to_owned(),
        };
        if validate_summary(&summary).is_err() {
            continue;
        }
        candidates.push(SkillCandidate {
            summary,
            rank: root.rank,
            locator: path.to_string_lossy().into_owned(),
        });
    }
    Ok(candidates)
}

#[derive(Deserialize)]
struct SkillFrontmatter {
    name: String,
    description: String,
    #[serde(
        default,
        rename = "when-to-use",
        alias = "whenToUse",
        alias = "when_to_use"
    )]
    when_to_use: Option<String>,
    #[serde(default, rename = "disable-model-invocation")]
    disable_model_invocation: Option<BooleanValue>,
    #[serde(default, rename = "user-invocable")]
    user_invocable: Option<BooleanValue>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum BooleanValue {
    Boolean(bool),
    String(String),
    Integer(i64),
}

impl BooleanValue {
    fn resolve(self, field: &str) -> Result<bool, HarnessError> {
        match self {
            Self::Boolean(value) => Ok(value),
            Self::Integer(0) => Ok(false),
            Self::Integer(1) => Ok(true),
            Self::Integer(_) => Err(HarnessError::invalid(format!(
                "skill frontmatter {field} must be boolean"
            ))),
            Self::String(value) => match value.trim().to_ascii_lowercase().as_str() {
                "true" | "yes" | "on" | "1" => Ok(true),
                "false" | "no" | "off" | "0" => Ok(false),
                _ => Err(HarnessError::invalid(format!(
                    "skill frontmatter {field} must be boolean"
                ))),
            },
        }
    }
}

struct ParsedSkill {
    name: String,
    description: String,
    when_to_use: Option<String>,
    invocation: SkillInvocationPolicy,
    content: String,
}

async fn parse_skill_file(path: &Path) -> Result<ParsedSkill, HarnessError> {
    let bytes = tokio::fs::read(path)
        .await
        .map_err(|error| io_error("read skill file", path, &error))?;
    if bytes.len() > MAX_SKILL_CONTENT_BYTES {
        return Err(HarnessError::invalid(format!(
            "skill file {} exceeds {MAX_SKILL_CONTENT_BYTES} bytes",
            path.display()
        )));
    }
    let text = String::from_utf8(bytes)
        .map_err(|_| HarnessError::invalid(format!("skill file {} is not UTF-8", path.display())))?
        .replace("\r\n", "\n");
    let (frontmatter, body) = split_frontmatter(&text)?;
    let parsed = if let Some(frontmatter) = frontmatter {
        let metadata: SkillFrontmatter = serde_yaml_ng::from_str(frontmatter).map_err(|error| {
            HarnessError::invalid(format!(
                "parse skill frontmatter {}: {error}",
                path.display()
            ))
        })?;
        let model_disabled = metadata
            .disable_model_invocation
            .map_or(Ok(false), |value| value.resolve("disable-model-invocation"))?;
        let user_invocable = metadata
            .user_invocable
            .map_or(Ok(true), |value| value.resolve("user-invocable"))?;
        ParsedSkill {
            name: metadata.name,
            description: metadata.description,
            when_to_use: metadata.when_to_use,
            invocation: SkillInvocationPolicy {
                model_invocable: !model_disabled,
                user_invocable,
            },
            content: body.trim().to_owned(),
        }
    } else {
        let name = if path.file_name() == Some(OsStr::new("SKILL.md")) {
            path.parent()
                .and_then(Path::file_name)
                .and_then(OsStr::to_str)
        } else {
            path.file_stem().and_then(OsStr::to_str)
        }
        .ok_or_else(|| HarnessError::invalid("skill file has no UTF-8 name"))?
        .to_owned();
        let description = text
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .map_or_else(
                || name.clone(),
                |line| line.trim_start_matches('#').trim().to_owned(),
            );
        ParsedSkill {
            name,
            description,
            when_to_use: None,
            invocation: SkillInvocationPolicy::default(),
            content: text.trim().to_owned(),
        }
    };
    validate_skill_name(&parsed.name)?;
    if parsed.description.trim().is_empty() || parsed.content.is_empty() {
        return Err(HarnessError::invalid(
            "skill name, description, and instruction body must not be empty",
        ));
    }
    Ok(parsed)
}

fn split_frontmatter(text: &str) -> Result<(Option<&str>, &str), HarnessError> {
    let Some(rest) = text.strip_prefix("---\n") else {
        return Ok((None, text));
    };
    let Some((frontmatter, body)) = rest.split_once("\n---\n") else {
        return Err(HarnessError::invalid(
            "skill YAML frontmatter has no closing --- line",
        ));
    };
    Ok((Some(frontmatter), body))
}

fn io_error(operation: &str, path: &Path, error: &std::io::Error) -> HarnessError {
    HarnessError::execution(format!("{operation} {}: {error}", path.display()))
}

struct SkillToolsPlugin;

impl HarnessPlugin for SkillToolsPlugin {
    fn descriptor(&self) -> &'static ComponentDescriptor {
        &TOOL_DESCRIPTOR
    }

    fn activate(&self, context: ComponentContext) -> Activation {
        let tools = context
            .context()
            .service::<Tools>()
            .expect("skill tools declares Tools");
        let skills = context
            .context()
            .service::<Skills>()
            .expect("skill tools declares Skills");
        Activation::Once(Box::pin(async move {
            let list = tools
                .register_tool(ToolRegistration {
                    spec: ToolSpec {
                        name: "list_skills".to_owned(),
                        description: "List the current winning model-invocable skills from all registered providers. Use this before loading a skill when its exact name is unknown.".to_owned(),
                        input_schema: json!({
                            "type": "object",
                            "properties": {},
                            "additionalProperties": false
                        }),
                    },
                    effect: ternilo_kernel::ToolEffect::ReadOnly,
                    handler: Arc::new(SkillTool {
                        skills: skills.clone(),
                        operation: SkillOperation::List,
                    }),
                })
                .await
                .map_err(|error| linorun_core::ActivationFailure::user(error.to_string()))?;
            let load = tools
                .register_tool(ToolRegistration {
                    spec: ToolSpec {
                        name: "skill".to_owned(),
                        description: "Load one complete model-invocable skill by its kebab-case catalog name. Follow the returned instructions and resolve relative resources against the supplied base directory.".to_owned(),
                        input_schema: json!({
                            "type": "object",
                            "properties": { "name": { "type": "string" } },
                            "required": ["name"],
                            "additionalProperties": false
                        }),
                    },
                    effect: ternilo_kernel::ToolEffect::ReadOnly,
                    handler: Arc::new(SkillTool {
                        skills,
                        operation: SkillOperation::Load,
                    }),
                })
                .await
                .map_err(|error| linorun_core::ActivationFailure::user(error.to_string()))?;
            Ok(Some(effect::inverse(move || async move {
                tools
                    .unregister_tool(load)
                    .await
                    .map_err(|error| CleanupError::user(error.to_string()))?;
                tools
                    .unregister_tool(list)
                    .await
                    .map_err(|error| CleanupError::user(error.to_string()))
            })))
        }))
    }
}

#[derive(Clone, Copy)]
enum SkillOperation {
    List,
    Load,
}

struct SkillTool {
    skills: SkillsClient,
    operation: SkillOperation,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LoadArguments {
    name: String,
}

impl ToolHandler for SkillTool {
    fn execute<'a>(
        &'a self,
        _: ToolExecutionContext,
        arguments: Value,
    ) -> Pin<Box<dyn Future<Output = Result<ToolOutput, HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            let content = match self.operation {
                SkillOperation::List => {
                    let _: EmptyConfig = serde_json::from_value(arguments).map_err(|error| {
                        HarnessError::invalid(format!("invalid list_skills arguments: {error}"))
                    })?;
                    let mut snapshot = self.skills.snapshot().await?;
                    snapshot
                        .skills
                        .retain(|skill| skill.invocation.model_invocable);
                    serde_json::to_string_pretty(&snapshot).map_err(|error| {
                        HarnessError::execution(format!("serialize skill catalog: {error}"))
                    })?
                }
                SkillOperation::Load => {
                    let arguments: LoadArguments =
                        serde_json::from_value(arguments).map_err(|error| {
                            HarnessError::invalid(format!("invalid skill arguments: {error}"))
                        })?;
                    validate_skill_name(&arguments.name)?;
                    let skill =
                        self.skills
                            .get(arguments.name.clone())
                            .await?
                            .ok_or_else(|| {
                                HarnessError::invalid(format!("unknown skill {:?}", arguments.name))
                            })?;
                    if !skill.summary.invocation.model_invocable {
                        return Err(HarnessError::policy(format!(
                            "skill {:?} is not model-invocable",
                            skill.summary.name
                        )));
                    }
                    render_skill_content(&skill)
                }
            };
            Ok(ToolOutput {
                content,
                is_error: false,
            })
        })
    }
}

#[must_use]
pub fn render_skill_content(skill: &SkillDefinition) -> String {
    let resources = skill.resource_base.as_ref().map_or_else(
        || {
            format!(
                "Resources are managed by provider \"{}\". Load referenced resources only as needed.",
                escape_text(&skill.summary.provider)
            )
        },
        |path| {
            format!(
                "Base directory: {}\nResolve relative paths against this directory and load only what is needed.",
                escape_text(path)
            )
        },
    );
    format!(
        "<skill_content name=\"{}\">\n<skill_resources>\n{}\n</skill_resources>\n\n<skill_instructions>\n{}\n</skill_instructions>\n</skill_content>",
        escape_attribute(&skill.summary.name),
        resources,
        skill.content
    )
}

pub fn prepare_skill_invocation(
    skill: &SkillDefinition,
    request: &str,
) -> Result<PreparedSkillInvocation, HarnessError> {
    if !skill.summary.invocation.user_invocable {
        return Err(HarnessError::policy(format!(
            "skill {:?} is not user-invocable",
            skill.summary.name
        )));
    }
    let request = request.trim();
    if request.chars().count() > 100_000 {
        return Err(HarnessError::invalid(
            "skill invocation request must not exceed 100000 characters",
        ));
    }
    let rendered = render_skill_content(skill);
    let (model_input, display_input) = if request.is_empty() {
        (
            format!(
                "The user explicitly invoked the following skill. Apply it to the current task.\n\n{rendered}"
            ),
            format!("/skill {}", skill.summary.name),
        )
    } else {
        (
            format!(
                "The user explicitly invoked the following skill. Follow it for the request after the skill block.\n\n{rendered}\n\n<user_request>\n{request}\n</user_request>"
            ),
            format!("/skill {}\n\n{request}", skill.summary.name),
        )
    };
    Ok(PreparedSkillInvocation {
        name: skill.summary.name.clone(),
        model_input,
        display_input,
    })
}

fn escape_attribute(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
}

fn escape_text(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;

    struct StaticProvider {
        candidate: SkillCandidate,
        definition: SkillDefinition,
    }

    impl SkillProvider for StaticProvider {
        fn list<'a>(
            &'a self,
        ) -> Pin<Box<dyn Future<Output = Result<SkillProviderObservation, HarnessError>> + Send + 'a>>
        {
            Box::pin(async move {
                Ok(SkillProviderObservation {
                    candidates: vec![self.candidate.clone()],
                    complete: true,
                })
            })
        }

        fn load<'a>(
            &'a self,
            _: String,
        ) -> Pin<Box<dyn Future<Output = Result<Option<SkillDefinition>, HarnessError>> + Send + 'a>>
        {
            Box::pin(async move { Ok(Some(self.definition.clone())) })
        }
    }

    fn provider(name: &str, source: &str, rank: u32, description: &str) -> StaticProvider {
        let summary = SkillSummary {
            name: "review-code".to_owned(),
            description: description.to_owned(),
            when_to_use: None,
            invocation: SkillInvocationPolicy::default(),
            source: source.to_owned(),
            provider: name.to_owned(),
        };
        StaticProvider {
            candidate: SkillCandidate {
                summary: summary.clone(),
                rank,
                locator: format!("{name}:review-code"),
            },
            definition: SkillDefinition {
                summary,
                content: "Review the code carefully.".to_owned(),
                resource_base: None,
            },
        }
    }

    #[tokio::test]
    async fn registry_resolves_rank_and_invalidates_cached_catalogs() {
        let registry = SkillRegistry::default();
        let high = registry
            .register_inner(SkillProviderRegistration {
                name: "user".to_owned(),
                provider: Arc::new(provider("user", "user", 400, "user copy")),
            })
            .unwrap();
        registry
            .register_inner(SkillProviderRegistration {
                name: "project".to_owned(),
                provider: Arc::new(provider("project", "project", 100, "project copy")),
            })
            .unwrap();

        let snapshot = registry.collect().await.unwrap();
        assert_eq!(snapshot.snapshot.skills[0].description, "project copy");
        let revision = snapshot.snapshot.revision;
        registry.invalidate_provider(high).unwrap();
        assert!(registry.collect().await.unwrap().snapshot.revision > revision);
    }

    #[tokio::test]
    async fn filesystem_frontmatter_controls_invocation_and_resource_base() {
        let root = std::env::temp_dir().join(format!(
            "ternilo-skill-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let bundle = root.join("review-code");
        tokio::fs::create_dir_all(&bundle).await.unwrap();
        tokio::fs::write(
            bundle.join("SKILL.md"),
            "---\nname: review-code\ndescription: Review a change\ndisable-model-invocation: false\nuser-invocable: yes\n---\nInspect every changed file.",
        )
        .await
        .unwrap();
        let provider = FilesystemSkillProvider {
            name: "fixture".to_owned(),
            roots: vec![SkillRoot {
                path: root.clone(),
                source: "custom",
                rank: 300,
                skip_system: false,
            }],
        };

        let observation = provider.discover().await;
        assert!(observation.complete);
        assert_eq!(observation.candidates.len(), 1);
        let definition = provider
            .load_locator(&observation.candidates[0].locator)
            .await
            .unwrap()
            .unwrap();
        assert!(definition.summary.invocation.model_invocable);
        assert!(definition.summary.invocation.user_invocable);
        assert_eq!(definition.content, "Inspect every changed file.");
        assert_eq!(
            definition.resource_base.as_deref(),
            bundle.canonicalize().unwrap().to_str()
        );

        tokio::fs::remove_dir_all(root).await.unwrap();
    }
}
