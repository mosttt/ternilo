use std::{
    collections::BTreeMap,
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
};

use linorun_core::{Activation, CallContext, ComponentContext, ComponentDescriptor};
use linorun_macros::component_descriptor;
use ternilo_kernel::{HarnessPlugin, PluginFactory, PluginManifest, Prompts, PromptsProvider};
use ternilo_protocol::{HarnessError, PromptSection};

use crate::{EmptyConfig, factory as make_factory, parse_config};

pub const KIND: &str = "ternilo.prompt.registry";

component_descriptor! {
    static DESCRIPTOR: () {
        id: "ternilo/builtin-prompt-registry@1",
        requires: [],
        provides: [Prompts],
    }
}

pub fn factory() -> PluginFactory {
    make_factory(
        PluginManifest {
            kind: KIND,
            requires: &[],
            provides: &["ternilo/prompts@1"],
        },
        |value| {
            let _: EmptyConfig = parse_config(value)?;
            Ok(Arc::new(PromptRegistryPlugin))
        },
    )
    .with_config_schema::<EmptyConfig>()
}

struct PromptRegistryPlugin;

impl HarnessPlugin for PromptRegistryPlugin {
    fn descriptor(&self) -> &'static ComponentDescriptor {
        &DESCRIPTOR
    }

    fn activate(&self, context: ComponentContext) -> Activation {
        let route = context.context().clone();
        let scope = context.scope().clone();
        let provider: Arc<dyn PromptsProvider> = Arc::new(PromptRegistry::default());
        Activation::Once(Box::pin(async move {
            scope
                .provide::<Prompts>(&route, provider)
                .await
                .map_err(|error| {
                    linorun_core::ActivationFailure::user(format!(
                        "provide prompt registry: {error}"
                    ))
                })?;
            Ok(None)
        }))
    }
}

#[derive(Default)]
struct PromptState {
    next: u64,
    sections: BTreeMap<u64, PromptSection>,
}

#[derive(Default)]
struct PromptRegistry {
    state: Mutex<PromptState>,
}

impl PromptsProvider for PromptRegistry {
    fn register<'a>(
        &'a self,
        _: CallContext<()>,
        section: PromptSection,
    ) -> Pin<Box<dyn Future<Output = Result<u64, HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            if section.id.trim().is_empty() || section.content.trim().is_empty() {
                return Err(HarnessError::invalid(
                    "prompt section id and content must not be empty",
                ));
            }
            let mut state = self
                .state
                .lock()
                .map_err(|_| HarnessError::execution("prompt registry lock poisoned"))?;
            if state.sections.values().any(|value| value.id == section.id) {
                return Err(HarnessError::composition(format!(
                    "prompt section {:?} is already registered",
                    section.id
                )));
            }
            let registration = state.next;
            state.next = state
                .next
                .checked_add(1)
                .ok_or_else(|| HarnessError::execution("prompt registration id exhausted"))?;
            state.sections.insert(registration, section);
            Ok(registration)
        })
    }

    fn unregister<'a>(
        &'a self,
        _: CallContext<()>,
        registration: u64,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            let removed = self
                .state
                .lock()
                .map_err(|_| HarnessError::execution("prompt registry lock poisoned"))?
                .sections
                .remove(&registration);
            removed.map(|_| ()).ok_or_else(|| {
                HarnessError::execution(format!("unknown prompt registration {registration}"))
            })
        })
    }

    fn assemble<'a>(
        &'a self,
        _: CallContext<()>,
    ) -> Pin<Box<dyn Future<Output = String> + Send + 'a>> {
        Box::pin(async move {
            let mut sections: Vec<_> = self
                .state
                .lock()
                .expect("prompt registry lock poisoned")
                .sections
                .values()
                .cloned()
                .collect();
            sections.sort_by(|left, right| {
                left.order
                    .cmp(&right.order)
                    .then_with(|| left.id.cmp(&right.id))
            });
            sections
                .into_iter()
                .map(|section| section.content)
                .collect::<Vec<_>>()
                .join("\n\n")
        })
    }
}
