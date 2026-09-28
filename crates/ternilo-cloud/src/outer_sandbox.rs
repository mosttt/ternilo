use std::{collections::BTreeMap, ffi::OsString, future::Future, path::Path, pin::Pin, sync::Arc};

use linorun_core::{Activation, CallContext, ComponentContext, ComponentDescriptor};
use linorun_macros::component_descriptor;
use serde::Deserialize;
use ternilo_kernel::{
    ConfinedCommand, HarnessPlugin, PluginFactory, PluginManifest, SandboxEnforcement, SandboxMode,
    SandboxPolicy, Sandboxes, SandboxesProvider,
};
use ternilo_protocol::HarnessError;

pub const OUTER_SANDBOX_KIND: &str = "ternilo.sandbox.cloud_outer";

component_descriptor! {
    static DESCRIPTOR: () {
        id: "ternilo/cloud-outer-sandbox@1",
        requires: [],
        provides: [Sandboxes],
    }
}

#[derive(Default, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct OuterSandboxConfig {}

#[must_use]
pub fn outer_sandbox_factory() -> PluginFactory {
    PluginFactory::new(
        PluginManifest {
            kind: OUTER_SANDBOX_KIND,
            requires: &[],
            provides: &["ternilo/sandbox@1"],
        },
        |value| {
            let _: OuterSandboxConfig = serde_json::from_value(if value.is_null() {
                serde_json::json!({})
            } else {
                value
            })
            .map_err(|error| {
                HarnessError::composition(format!(
                    "invalid cloud outer sandbox configuration: {error}"
                ))
            })?;
            Ok(Arc::new(OuterSandboxPlugin))
        },
    )
    .with_description("复用 Worker 已建立的外层 namespace 隔离执行 Workspace shell。")
    .with_config_schema::<OuterSandboxConfig>()
}

struct OuterSandboxPlugin;

impl HarnessPlugin for OuterSandboxPlugin {
    fn descriptor(&self) -> &'static ComponentDescriptor {
        &DESCRIPTOR
    }

    fn activate(&self, context: ComponentContext) -> Activation {
        let route = context.context().clone();
        let scope = context.scope().clone();
        Activation::Once(Box::pin(async move {
            let provider: Arc<dyn SandboxesProvider> = Arc::new(OuterSandbox);
            scope
                .provide::<Sandboxes>(&route, provider)
                .await
                .map_err(|error| {
                    linorun_core::ActivationFailure::user(format!(
                        "provide cloud outer sandbox: {error}"
                    ))
                })?;
            Ok(None)
        }))
    }
}

struct OuterSandbox;

impl SandboxesProvider for OuterSandbox {
    fn confine<'a>(
        &'a self,
        _: CallContext<()>,
        program: OsString,
        arguments: Vec<OsString>,
        policy: SandboxPolicy,
    ) -> Pin<Box<dyn Future<Output = Result<ConfinedCommand, HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            if std::env::var_os("TERNILO_OUTER_SANDBOX").as_deref()
                != Some(std::ffi::OsStr::new("1"))
            {
                return Err(HarnessError::policy(
                    "cloud shell requires a namespace-isolated Worker child",
                ));
            }
            if policy.mode != SandboxMode::WorkspaceWrite
                || policy.workspace_root != Path::new("/workspace")
            {
                return Err(HarnessError::policy(
                    "cloud outer sandbox is valid only for the mounted /workspace boundary",
                ));
            }
            if program.is_empty() {
                return Err(HarnessError::invalid(
                    "sandbox command program must not be empty",
                ));
            }
            Ok(ConfinedCommand {
                program,
                arguments,
                environment: BTreeMap::from([
                    (OsString::from("HOME"), OsString::from("/workspace")),
                    (
                        OsString::from("PATH"),
                        OsString::from("/usr/local/bin:/usr/bin:/bin"),
                    ),
                    (OsString::from("LANG"), OsString::from("C.UTF-8")),
                ]),
                backend: "cloud-worker-outer".to_owned(),
                enforcement: SandboxEnforcement::Full,
            })
        })
    }
}
