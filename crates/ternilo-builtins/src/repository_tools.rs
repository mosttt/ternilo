use linorun_core::{Activation, CleanupError, ComponentContext, ComponentDescriptor, effect};
use linorun_macros::component_descriptor;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{future::Future, pin::Pin, sync::Arc};
use ternilo_kernel::{
    HarnessPlugin, PluginFactory, PluginManifest, Prompts, Shell, ShellClient, ToolEffect,
    ToolExecutionContext, ToolHandler, ToolRegistration, Tools,
};
use ternilo_protocol::{HarnessError, PromptSection, ShellRequest, ToolOutput, ToolSpec};

pub const KIND: &str = "ternilo.tools.repository";
component_descriptor! { static DESCRIPTOR: () { id: "ternilo/builtin-repository-tools@1", requires: [Tools, Shell, Prompts], provides: [], } }
pub fn factory() -> PluginFactory {
    crate::factory(
        PluginManifest {
            kind: KIND,
            requires: &["ternilo/tools@1", "ternilo/shell@1", "ternilo/prompts@1"],
            provides: &[],
        },
        |value| {
            let _: crate::EmptyConfig = crate::parse_config(value)?;
            Ok(Arc::new(Plugin))
        },
    )
    .with_config_schema::<crate::EmptyConfig>()
}
struct Plugin;
impl HarnessPlugin for Plugin {
    fn descriptor(&self) -> &'static ComponentDescriptor {
        &DESCRIPTOR
    }
    fn activate(&self, context: ComponentContext) -> Activation {
        let tools = context
            .context()
            .service::<Tools>()
            .expect("repository tools declare tools");
        let shell = context
            .context()
            .service::<Shell>()
            .expect("repository tools declare shell");
        let prompts = context
            .context()
            .service::<Prompts>()
            .expect("repository tools declare prompts");
        Activation::Once(Box::pin(async move {
            let mut registrations = Vec::new();
            for commit in [false, true] {
                let spec = if commit {
                    ToolSpec { name: "git_commit_paths".into(), description: "Stage and commit only explicitly selected paths, preserving unrelated staged files. Inspect changes and run relevant checks first. Does not push, reset, clean or delete branches. Git must be installed; repository hooks still run.".into(), input_schema: json!({"type":"object","required":["paths","message"],"properties":{"paths":{"type":"array","minItems":1,"maxItems":64,"items":{"type":"string"}},"message":{"type":"string"}},"additionalProperties":false}) }
                } else {
                    ToolSpec { name: "git_inspect".into(), description: "Inspect workspace Git status, diff, log or a commit without changing files. External diff programs and text conversion are disabled. Git must be installed.".into(), input_schema: json!({"type":"object","properties":{"action":{"type":"string","enum":["status","diff","log","show"]},"reference":{"type":"string"},"staged":{"type":"boolean"},"paths":{"type":"array","maxItems":64,"items":{"type":"string"}},"limit":{"type":"integer","minimum":1,"maximum":100}},"additionalProperties":false}) }
                };
                registrations.push(
                    tools
                        .register_tool(ToolRegistration {
                            spec,
                            effect: if commit {
                                ToolEffect::Mutating
                            } else {
                                ToolEffect::ReadOnly
                            },
                            handler: Arc::new(RepositoryTool {
                                shell: shell.clone(),
                                commit,
                            }),
                        })
                        .await
                        .map_err(|error| {
                            linorun_core::ActivationFailure::user(error.to_string())
                        })?,
                );
            }
            let prompt = prompts.register(PromptSection { id: "repository-workflow".into(), order: 260, content: "Repository workflow: inspect status and diff before editing or committing; preserve unrelated user changes. Run the project's relevant checks, then use git_commit_paths only for intended paths. Review the exact pull request head, changed files and checks before proposing a merge. Publishing or merging must follow the user's task authorization; a successful local check is not a completed remote check.".into() }).await.map_err(|error| linorun_core::ActivationFailure::user(error.to_string()))?;
            Ok(Some(effect::inverse(move || async move {
                for id in registrations {
                    tools
                        .unregister_tool(id)
                        .await
                        .map_err(|error| CleanupError::user(error.to_string()))?;
                }
                prompts
                    .unregister(prompt)
                    .await
                    .map_err(|error| CleanupError::user(error.to_string()))
            })))
        }))
    }
}
#[derive(Default, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Action {
    #[default]
    Status,
    Diff,
    Log,
    Show,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Inspect {
    #[serde(default)]
    action: Action,
    reference: Option<String>,
    #[serde(default)]
    staged: bool,
    #[serde(default)]
    paths: Vec<String>,
    #[serde(default = "default_limit")]
    limit: u32,
}
const fn default_limit() -> u32 {
    20
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Commit {
    paths: Vec<String>,
    message: String,
}
struct RepositoryTool {
    shell: ShellClient,
    commit: bool,
}

fn validate_paths(paths: &[String]) -> Result<(), HarnessError> {
    if paths.len() > 64
        || paths.iter().any(|path| {
            path.is_empty() || path.chars().any(char::is_control) || path.starts_with(':')
        })
    {
        return Err(HarnessError::invalid(
            "Git paths must be nonempty literal paths, without control characters or pathspec magic; at most 64 paths are allowed",
        ));
    }
    Ok(())
}
#[cfg(not(windows))]
fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}
#[cfg(windows)]
fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}
fn git_command(args: &[String], paths: &[String], inspect: bool) -> String {
    let mut all = vec![
        "git".to_owned(),
        "--literal-pathspecs".into(),
        "--no-pager".into(),
    ];
    if inspect {
        all.extend([
            "--no-optional-locks".into(),
            "-c".into(),
            "core.fsmonitor=false".into(),
        ]);
    }
    all.extend_from_slice(args);
    all.push("--".into());
    all.extend_from_slice(paths);
    let command = all
        .iter()
        .map(|part| quote(part))
        .collect::<Vec<_>>()
        .join(" ");
    #[cfg(windows)]
    let command = format!("& {command}");
    command
}
fn inspect_command(input: Inspect) -> Result<String, HarnessError> {
    validate_paths(&input.paths)?;
    if input.reference.as_ref().is_some_and(|value| {
        value.is_empty() || value.starts_with('-') || value.chars().any(char::is_control)
    }) {
        return Err(HarnessError::invalid(
            "Git reference must not be empty, an option, or contain control characters",
        ));
    }
    let mut args: Vec<String> = match input.action {
        Action::Status => vec!["status".into(), "--short".into(), "--branch".into()],
        Action::Diff => vec![
            "diff".into(),
            "--no-ext-diff".into(),
            "--no-textconv".into(),
        ],
        Action::Log => vec![
            "log".into(),
            format!("-{}", input.limit.clamp(1, 100)),
            "--format=%H %s".into(),
        ],
        Action::Show => vec![
            "show".into(),
            "--no-ext-diff".into(),
            "--no-textconv".into(),
            "--format=fuller".into(),
        ],
    };
    if matches!(input.action, Action::Diff) && input.staged {
        args.push("--cached".into());
    }
    if !matches!(input.action, Action::Status)
        && let Some(reference) = input.reference
    {
        args.push(reference);
    }
    Ok(git_command(&args, &input.paths, true))
}
impl ToolHandler for RepositoryTool {
    fn execute<'a>(
        &'a self,
        context: ToolExecutionContext,
        arguments: Value,
    ) -> Pin<Box<dyn Future<Output = Result<ToolOutput, HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            let commands = if self.commit {
                let input: Commit = serde_json::from_value(arguments)
                    .map_err(|error| HarnessError::invalid(error.to_string()))?;
                validate_paths(&input.paths)?;
                if input.paths.is_empty()
                    || input.message.trim().is_empty()
                    || input.message.len() > 16_384
                    || input.message.contains('\0')
                {
                    return Err(HarnessError::invalid(
                        "a commit requires explicit paths and a nonempty message up to 16 KiB",
                    ));
                }
                vec![
                    (git_command(&["add".into()], &input.paths, false), None),
                    (
                        git_command(
                            &["commit".into(), "--only".into(), "--file=-".into()],
                            &input.paths,
                            false,
                        ),
                        Some(input.message),
                    ),
                ]
            } else {
                vec![(
                    inspect_command(
                        serde_json::from_value(arguments)
                            .map_err(|error| HarnessError::invalid(error.to_string()))?,
                    )?,
                    None,
                )]
            };
            let mut results = Vec::new();
            let mut is_error = false;
            for (command, stdin) in commands {
                context.cancellation.check()?;
                let result = self
                    .shell
                    .execute(
                        context.run_id.clone(),
                        ShellRequest {
                            command,
                            timeout_ms: 60_000,
                            full_access: false,
                            stdin,
                            env: [("GIT_TERMINAL_PROMPT".into(), "0".into())].into(),
                        },
                    )
                    .await?;
                is_error = result.exit_code != Some(0) || result.timed_out;
                results.push(result);
                if is_error {
                    break;
                }
            }
            Ok(ToolOutput {
                content: serde_json::to_string(&json!({"operations":results}))
                    .map_err(|error| HarnessError::execution(error.to_string()))?,
                is_error,
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn arguments_are_literal_and_unrelated_staged_files_stay_out_of_a_selected_commit() {
        let root = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            let result = std::process::Command::new("git")
                .args(args)
                .current_dir(root.path())
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "{}",
                String::from_utf8_lossy(&result.stderr)
            );
            String::from_utf8_lossy(&result.stdout).into_owned()
        };
        git(&["init", "-q"]);
        git(&["config", "user.name", "Fixture"]);
        git(&["config", "user.email", "fixture@example.invalid"]);
        std::fs::write(root.path().join("selected.txt"), "selected").unwrap();
        std::fs::write(root.path().join("unrelated.txt"), "unrelated").unwrap();
        git(&["add", "--", "selected.txt", "unrelated.txt"]);
        git(&[
            "commit",
            "--only",
            "-m",
            "selected commit",
            "--",
            "selected.txt",
        ]);
        assert_eq!(
            git(&["diff", "--cached", "--name-only"]).trim(),
            "unrelated.txt"
        );
        assert!(
            inspect_command(Inspect {
                reference: Some("--output=bad".into()),
                ..serde_json::from_value(json!({})).unwrap()
            })
            .is_err()
        );
        let literal = "a file $(touch BAD).rs";
        std::fs::write(root.path().join(literal), "literal path evidence").unwrap();
        git(&["add", "--", literal]);
        let command = inspect_command(
            serde_json::from_value(json!({"action":"diff","staged":true,"paths":[literal]}))
                .unwrap(),
        )
        .unwrap();
        #[cfg(not(windows))]
        let result = std::process::Command::new("sh")
            .arg("-c")
            .arg(command)
            .current_dir(root.path())
            .output()
            .unwrap();
        #[cfg(windows)]
        let result = std::process::Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command", &command])
            .current_dir(root.path())
            .output()
            .unwrap();
        assert!(result.status.success());
        assert!(String::from_utf8_lossy(&result.stdout).contains("literal path evidence"));
        assert!(!root.path().join("BAD").exists());
    }
}
