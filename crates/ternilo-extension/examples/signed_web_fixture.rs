use std::{collections::BTreeSet, env, fs, path::PathBuf};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use ed25519_dalek::SigningKey;
use serde_json::json;
use ternilo_extension::{
    Capability, EXTENSION_PACKAGE_SCHEMA_VERSION, ExtensionCommandContribution,
    ExtensionCommandInput, ExtensionContributions, ExtensionHookContribution, ExtensionHookMatcher,
    ExtensionManifest, ExtensionPayload, ExtensionPromptSectionContribution,
    ExtensionProviderContribution, ExtensionProviderCredential, ExtensionRuntime,
    ExtensionSkillContribution, ExtensionToolContribution, ExtensionToolEffect, PublisherTrust,
    RhaiExecutionLimits, WASM_COMPONENT_RUNTIME_WORLD, WasmComponentLimits, sign_bundle,
};
use ternilo_protocol::{
    HookPoint, ProviderModel, ProviderModelDefaults, ProviderModelSettings, ProviderProtocol,
    SkillInvocationPolicy, ToolPresentationDescriptor, ToolPresentationField,
    ToolPresentationIconKind, ToolPresentationResultDescriptor, ToolPresentationResultKind,
    ToolSpec,
};

const COMPONENT: &str = r#"
    (component
      (type $host (instance
        (type (enum "debug" "info" "warn" "error"))
        (export "log-level" (type (eq 0)))
        (type (result (error string)))
        (type (func
          (param "level" 1)
          (param "message" string)
          (result 2)))
        (export "log" (func (type 3)))
        (type (result string (error string)))
        (type (func
          (param "path" string)
          (result 4)))
        (export "read-workspace-text" (func (type 5)))))
      (import "ternilo:extension/host@1.0.0" (instance $host-import (type $host)))

      (core module $guest
        (memory (export "memory") 1)
        (global $heap (mut i32) (i32.const 4096))
        (data (i32.const 2048) "{\"content\":\"[{\\\"status\\\":\\\"signed\\\",\\\"message\\\":\\\"hello from fixture\\\"}]\",\"is_error\":false}")
        (func (export "cabi_realloc")
          (param $old i32) (param $old-size i32) (param $align i32) (param $new-size i32)
          (result i32)
          (local $result i32)
          (global.get $heap)
          (local.set $result)
          (global.get $heap)
          (local.get $new-size)
          (i32.add)
          (global.set $heap)
          (local.get $result))
        (func (export "cabi_post_invoke") (param i32))
        (func (export "invoke") (param i32 i32 i32 i32 i32 i32) (result i32)
          (i32.const 1024)
          (i32.const 0)
          (i32.store)
          (i32.const 1028)
          (i32.const 2048)
          (i32.store)
          (i32.const 1032)
          (i32.const 91)
          (i32.store)
          (i32.const 1024)))
      (core instance $guest-instance (instantiate $guest))
      (func $invoke
        (param "handler" string)
        (param "context-json" string)
        (param "input-json" string)
        (result (result string (error string)))
        (canon lift (core func $guest-instance "invoke")
          (memory (core memory $guest-instance "memory"))
          (realloc (core func $guest-instance "cabi_realloc"))
          (post-return (core func $guest-instance "cabi_post_invoke"))))
      (export "invoke" (func $invoke)))
"#;

const RHAI_SOURCE: &str = r#"
fn signed_rhai(context, arguments, settings) {
    #{
        content: settings.prefix + arguments.subject + " (" + context.session_id + ")",
        is_error: false
    }
}

fn guard_fixture(context, arguments, settings) {
    #{}
}
"#;

#[expect(
    clippy::too_many_lines,
    reason = "Keep the signed browser fixture packages and their shared output manifest together."
)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let output = env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .ok_or("usage: signed_web_fixture <output-directory>")?;
    fs::create_dir_all(&output)?;

    let signing_key = SigningKey::from_bytes(&[29; 32]);
    let source = "https://plugins.ternilo.dev/browser-fixture".to_owned();
    let publisher = PublisherTrust {
        key_id: "ternilo-browser-fixture".to_owned(),
        public_key_base64: STANDARD.encode(signing_key.verifying_key().to_bytes()),
        allowed_sources: BTreeSet::from([source.clone()]),
    };
    let presentation = ToolPresentationDescriptor {
        title: "Signed fixture report".to_owned(),
        icon_kind: ToolPresentationIconKind::Sparkles,
        input_summary: vec![ToolPresentationField {
            label: "Subject".to_owned(),
            path: vec!["subject".to_owned()],
        }],
        result: ToolPresentationResultDescriptor {
            kind: ToolPresentationResultKind::Table,
            columns: vec![
                ToolPresentationField {
                    label: "Trust".to_owned(),
                    path: vec!["status".to_owned()],
                },
                ToolPresentationField {
                    label: "Message".to_owned(),
                    path: vec!["message".to_owned()],
                },
            ],
        },
    };
    let manifest = ExtensionManifest {
        schema_version: EXTENSION_PACKAGE_SCHEMA_VERSION,
        package_id: "dev.ternilo.browser-fixture".to_owned(),
        version: "1.0.0".to_owned(),
        description: Some("Signed browser acceptance fixture".to_owned()),
        source,
        publisher_key_id: publisher.key_id.clone(),
        payload_sha256: "0".repeat(64),
        runtime: ExtensionRuntime::WasmComponent {
            world: WASM_COMPONENT_RUNTIME_WORLD.to_owned(),
            limits: WasmComponentLimits {
                fuel: 100_000,
                max_memory_bytes: 2 * 1024 * 1024,
                max_input_bytes: 16 * 1024,
                max_output_bytes: 16 * 1024,
                max_workspace_read_bytes: 16 * 1024,
            },
        },
        config_schema: json!({
            "type": "object",
            "properties": {
                "salutation": {
                    "type": "string",
                    "title": "Fixture salutation",
                    "default": "hello"
                }
            },
            "additionalProperties": false
        }),
        contributions: ExtensionContributions {
            tools: vec![ExtensionToolContribution {
                handler: "report".to_owned(),
                spec: ToolSpec {
                    name: "signed_fixture".to_owned(),
                    description: "Return a deterministic result from a signed WASM Component."
                        .to_owned(),
                    input_schema: json!({
                        "type": "object",
                        "properties": { "subject": { "type": "string" } },
                        "required": ["subject"],
                        "additionalProperties": false
                    }),
                },
                output_schema: json!({
                    "type": "object",
                    "properties": {
                        "content": { "type": "string" },
                        "is_error": { "type": "boolean" }
                    },
                    "required": ["content", "is_error"],
                    "additionalProperties": false
                }),
                effect: ExtensionToolEffect::Dangerous,
                presentation: Some(presentation),
            }],
            prompt_sections: vec![ExtensionPromptSectionContribution {
                id: "fixture-guidance".to_owned(),
                order: 450,
                content: "Use the signed WASM fixture only for deterministic acceptance checks."
                    .to_owned(),
            }],
            skills: vec![ExtensionSkillContribution {
                name: "signed-extension-fixture".to_owned(),
                description: "Use a signed Extension fixture for browser acceptance checks."
                    .to_owned(),
                when_to_use: Some(
                    "Use when verifying signed Extension installation and catalog loading."
                        .to_owned(),
                ),
                invocation: SkillInvocationPolicy::default(),
                content: "Use the mounted signed Extension fixture only for deterministic browser acceptance checks. Verify its signature before invoking any contributed tool."
                    .to_owned(),
            }],
            hooks: Vec::new(),
            commands: Vec::new(),
            providers: Vec::new(),
        },
        requested_capabilities: BTreeSet::from([Capability::Log, Capability::WorkspaceRead]),
    };
    let bundle = sign_bundle(
        manifest,
        ExtensionPayload::Base64(STANDARD.encode(COMPONENT.as_bytes())),
        &signing_key,
    )?;
    fs::write(
        output.join("publisher.json"),
        serde_json::to_vec_pretty(&publisher)?,
    )?;
    fs::write(
        output.join("bundle.json"),
        serde_json::to_vec_pretty(&bundle)?,
    )?;
    let rhai_manifest = ExtensionManifest {
        schema_version: EXTENSION_PACKAGE_SCHEMA_VERSION,
        package_id: "dev.ternilo.browser-rhai-fixture".to_owned(),
        version: "1.0.0".to_owned(),
        description: Some("Signed Rhai browser acceptance fixture".to_owned()),
        source: "https://plugins.ternilo.dev/browser-fixture".to_owned(),
        publisher_key_id: publisher.key_id.clone(),
        payload_sha256: "0".repeat(64),
        runtime: ExtensionRuntime::Rhai {
            limits: RhaiExecutionLimits::default(),
        },
        config_schema: json!({
            "type": "object",
            "properties": {
                "prefix": {
                    "type": "string",
                    "title": "Rhai response prefix",
                    "default": "Signed Rhai: "
                }
            },
            "required": ["prefix"],
            "additionalProperties": false
        }),
        contributions: ExtensionContributions {
            tools: vec![ExtensionToolContribution {
                handler: "signed_rhai".to_owned(),
                spec: ToolSpec {
                    name: "signed_rhai_fixture".to_owned(),
                    description: "Return a deterministic result from a signed Rhai extension."
                        .to_owned(),
                    input_schema: json!({
                        "type": "object",
                        "properties": { "subject": { "type": "string" } },
                        "required": ["subject"],
                        "additionalProperties": false
                    }),
                },
                output_schema: json!({
                    "type": "object",
                    "properties": {
                        "content": { "type": "string" },
                        "is_error": { "const": false }
                    },
                    "required": ["content", "is_error"],
                    "additionalProperties": false
                }),
                effect: ExtensionToolEffect::ReadOnly,
                presentation: Some(ToolPresentationDescriptor {
                    title: "Signed Rhai response".to_owned(),
                    icon_kind: ToolPresentationIconKind::Code,
                    input_summary: vec![ToolPresentationField {
                        label: "Subject".to_owned(),
                        path: vec!["subject".to_owned()],
                    }],
                    result: ToolPresentationResultDescriptor {
                        kind: ToolPresentationResultKind::Text,
                        columns: Vec::new(),
                    },
                }),
            }],
            prompt_sections: vec![ExtensionPromptSectionContribution {
                id: "fixture-guidance".to_owned(),
                order: 450,
                content: "Use the signed Rhai fixture only for deterministic acceptance checks."
                    .to_owned(),
            }],
            skills: vec![ExtensionSkillContribution {
                name: "signed-extension-fixture".to_owned(),
                description: "Use a signed Extension fixture for browser acceptance checks."
                    .to_owned(),
                when_to_use: Some(
                    "Use when verifying signed Extension installation and catalog loading."
                        .to_owned(),
                ),
                invocation: SkillInvocationPolicy::default(),
                content: "Use the mounted signed Extension fixture only for deterministic browser acceptance checks. Verify its signature before invoking any contributed tool."
                    .to_owned(),
            }],
            hooks: vec![ExtensionHookContribution {
                id: "guard-fixture".to_owned(),
                point: HookPoint::PreToolUse,
                handler: "guard_fixture".to_owned(),
                matcher: ExtensionHookMatcher::ToolNames {
                    names: vec!["fixture-never-invoked".to_owned()],
                },
            }],
            commands: vec![ExtensionCommandContribution {
                name: "signed-fixture".to_owned(),
                description: "Invoke the signed Rhai fixture tool.".to_owned(),
                tool: "signed_rhai_fixture".to_owned(),
                input: Some(ExtensionCommandInput {
                    hint: "<subject>".to_owned(),
                    field: "subject".to_owned(),
                    images: false,
                }),
                fixed_arguments: json!({ "source": "signed-browser-fixture" }),
            }],
            providers: vec![ExtensionProviderContribution {
                id: "signed-fixture-provider".to_owned(),
                display_name: "Signed Fixture Provider".to_owned(),
                base_url: "https://api.example.test/v1".to_owned(),
                protocol: ProviderProtocol::OpenAiResponses,
                defaults: ProviderModelDefaults {
                    context_window: 128_000,
                    max_output_tokens: 16_384,
                    reasoning: None,
                },
                models: vec![ProviderModel {
                    id: "signed-fixture-model".to_owned(),
                    display_name: Some("Signed Fixture Model".to_owned()),
                    settings: ProviderModelSettings::Inherit,
                }],
                timeout_ms: 120_000,
                max_attempts: 3,
                retry_base_delay_ms: 250,
                credential: ExtensionProviderCredential {
                    required: true,
                    suggested_ref: Some("SIGNED_FIXTURE_API_KEY".to_owned()),
                },
            }],
        },
        requested_capabilities: BTreeSet::new(),
    };
    let rhai_bundle = sign_bundle(
        rhai_manifest,
        ExtensionPayload::Utf8(RHAI_SOURCE.to_owned()),
        &signing_key,
    )?;
    fs::write(
        output.join("rhai-bundle.json"),
        serde_json::to_vec_pretty(&rhai_bundle)?,
    )?;
    Ok(())
}
