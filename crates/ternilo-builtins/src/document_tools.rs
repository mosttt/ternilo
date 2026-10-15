use linorun_core::{Activation, CleanupError, ComponentContext, ComponentDescriptor, effect};
use linorun_macros::component_descriptor;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use std::fmt::Write as _;
use std::{
    future::Future,
    path::Path,
    pin::Pin,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};
use ternilo_kernel::{
    Attachments, AttachmentsClient, HarnessPlugin, PluginFactory, PluginManifest, Prompts,
    Sandboxes, SandboxesClient, Sessions, SessionsClient, ToolEffect, ToolExecutionContext,
    ToolHandler, ToolRegistration, Tools, WorkspaceFiles, WorkspaceFilesClient,
};
use ternilo_protocol::{
    Attachment, DocumentRender, DocumentReview, DocumentVerdict, FileReadRequest, HarnessError,
    PromptSection, SessionEventKind, ToolOutput, ToolSpec,
};

pub const KIND: &str = "ternilo.tools.document_review";
component_descriptor! { static DESCRIPTOR: () { id: "ternilo/builtin-document-review@1", requires: [Tools, WorkspaceFiles, Attachments, Sessions, Sandboxes, Prompts], provides: [], } }
#[derive(Clone, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct Config {
    #[serde(default = "default_browser")]
    browser_command: String,
}
fn default_browser() -> String {
    "chromium".into()
}
pub fn factory() -> PluginFactory {
    crate::factory(PluginManifest { kind: KIND, requires: &["ternilo/tools@1", "ternilo/workspace-files@2", "ternilo/attachments@1", "ternilo/sessions@1", "ternilo/sandbox@1", "ternilo/prompts@1"], provides: &[] }, |value| {
        let config: Config = crate::parse_config(value)?;
        if config.browser_command.trim().is_empty() || config.browser_command.chars().any(char::is_control) { return Err(HarnessError::invalid("document browser must be an executable name or path without control characters")); }
        Ok(Arc::new(Plugin { config }))
    }).with_config_schema::<Config>()
}
struct Plugin {
    config: Config,
}
impl HarnessPlugin for Plugin {
    fn descriptor(&self) -> &'static ComponentDescriptor {
        &DESCRIPTOR
    }
    fn activate(&self, context: ComponentContext) -> Activation {
        let tools = context
            .context()
            .service::<Tools>()
            .expect("documents declare tools");
        let files = context
            .context()
            .service::<WorkspaceFiles>()
            .expect("documents declare files");
        let attachments = context
            .context()
            .service::<Attachments>()
            .expect("documents declare attachments");
        let sessions = context
            .context()
            .service::<Sessions>()
            .expect("documents declare sessions");
        let sandboxes = context
            .context()
            .service::<Sandboxes>()
            .expect("documents declare sandboxes");
        let prompts = context
            .context()
            .service::<Prompts>()
            .expect("documents declare prompts");
        let config = self.config.clone();
        Activation::Once(Box::pin(async move {
            let mut registrations = Vec::new();
            for review in [false, true] {
                let spec = if review {
                    ToolSpec { name: "record_document_review".into(), description: "Record your visual review of the latest rendered document. Inspect every screenshot, record findings and use needs_changes when corrections are needed. Pass is rejected if the source changed or the renderer detected overflow, missing images or blocked resources. A render alone is not a completed review.".into(), input_schema: json!({"type":"object","required":["render_id","verdict","findings"],"properties":{"render_id":{"type":"string"},"verdict":{"type":"string","enum":["pass","needs_changes"]},"findings":{"type":"array","minItems":1,"maxItems":20,"items":{"type":"string"}}},"additionalProperties":false}) }
                } else {
                    ToolSpec { name: "render_document".into(), description: "Actually render a workspace Markdown (including $inline$ and $$display$$ math) or static HTML document in an isolated Chrome/Chromium process. Returns layout evidence and persists viewport screenshots in the model context for visual inspection. Document JavaScript and external/file resources are blocked; embed assets as data URLs. Math is rendered offline as MathML; invalid formulas and math commands that load resources or inject HTML are rejected. Supports at most 512 KiB source and eight screenshot pages. Requires a configured browser on the execution host and an image-capable model. Correct findings in the source, re-render, then record_document_review before declaring delivery verified.".into(), input_schema: json!({"type":"object","required":["path"],"properties":{"path":{"type":"string"},"width":{"type":"integer","minimum":320,"maximum":1920,"default":1024},"height":{"type":"integer","minimum":480,"maximum":1600,"default":900}},"additionalProperties":false}) }
                };
                registrations.push(
                    tools
                        .register_tool(ToolRegistration {
                            spec,
                            effect: ToolEffect::ReadOnly,
                            handler: Arc::new(DocumentTool {
                                review,
                                files: files.clone(),
                                attachments: attachments.clone(),
                                sessions: sessions.clone(),
                                sandboxes: sandboxes.clone(),
                                config: config.clone(),
                            }),
                        })
                        .await
                        .map_err(|error| {
                            linorun_core::ActivationFailure::user(error.to_string())
                        })?,
                );
            }
            let prompt = prompts.register(PromptSection { id: "document-delivery-workflow".into(), order: 270, content: "Document delivery workflow: generate the document, call render_document, inspect all actual screenshots and layout evidence, correct the source, and render again after changes. Use record_document_review with concrete findings tied to the latest source. Do not claim visual verification from source inspection alone. If a browser, supported format or image-capable model is unavailable, state that visual verification is pending.".into() }).await.map_err(|error| linorun_core::ActivationFailure::user(error.to_string()))?;
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
struct DocumentTool {
    review: bool,
    files: WorkspaceFilesClient,
    attachments: AttachmentsClient,
    sessions: SessionsClient,
    sandboxes: SandboxesClient,
    config: Config,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RenderArguments {
    path: String,
    #[serde(default = "default_width")]
    width: u32,
    #[serde(default = "default_height")]
    height: u32,
}
const fn default_width() -> u32 {
    1024
}
const fn default_height() -> u32 {
    900
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReviewArguments {
    render_id: String,
    verdict: DocumentVerdict,
    findings: Vec<String>,
}
fn digest(content: &str) -> String {
    Sha256::digest(content.as_bytes())
        .iter()
        .fold(String::with_capacity(64), |mut result, byte| {
            write!(result, "{byte:02x}").expect("writing to a String cannot fail");
            result
        })
}

async fn read_document(files: &WorkspaceFilesClient, path: &str) -> Result<String, HarnessError> {
    let mut content = String::new();
    let mut next = 1;
    loop {
        let part = files
            .read_text(FileReadRequest {
                path: path.to_owned(),
                start_line: Some(next),
                line_count: Some(2000),
            })
            .await?;
        if !content.is_empty() {
            content.push('\n');
        }
        content.push_str(&part.content);
        if content.len() > 512 * 1024 {
            return Err(HarnessError::invalid(
                "document source exceeds 512 KiB; split it before rendering",
            ));
        }
        if part.end_line >= part.total_lines || part.end_line < next {
            break;
        }
        next = part.end_line + 1;
    }
    if content.trim().is_empty() {
        return Err(HarnessError::invalid("document source is empty"));
    }
    Ok(content)
}
fn render_html(path: &str, source: &str) -> Result<String, HarnessError> {
    let extension = Path::new(path)
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let body = match extension.as_str() {
        "md" | "markdown" => {
            let mut output = String::new();
            let options = pulldown_cmark::Options::ENABLE_TABLES
                | pulldown_cmark::Options::ENABLE_STRIKETHROUGH
                | pulldown_cmark::Options::ENABLE_TASKLISTS
                | pulldown_cmark::Options::ENABLE_FOOTNOTES
                | pulldown_cmark::Options::ENABLE_MATH;
            pulldown_cmark::html::push_html(
                &mut output,
                pulldown_cmark::Parser::new_ext(source, options).map(|event| match event {
                    pulldown_cmark::Event::InlineMath(tex) => math_placeholder(&tex, false),
                    pulldown_cmark::Event::DisplayMath(tex) => math_placeholder(&tex, true),
                    event => event,
                }),
            );
            format!(
                "<style>body{{font:16px/1.65 sans-serif;padding:24px;margin:0;overflow-wrap:anywhere}}img,svg{{max-width:100%}}pre{{white-space:pre-wrap;background:#f4f4f4;padding:12px}}table{{border-collapse:collapse;max-width:100%}}th,td{{border:1px solid #ddd;padding:6px 10px}}h1,h2,h3{{line-height:1.3}}[data-ternilo-math=display]{{display:block;margin:1em 0}}math{{font-size:1.1em}}</style>{output}"
            )
        }
        "html" | "htm" => source.to_owned(),
        _ => {
            return Err(HarnessError::invalid(
                "document rendering supports Markdown and static HTML",
            ));
        }
    };
    Ok(format!(
        "<!doctype html><meta charset=\"utf-8\"><meta name=\"ternilo-document-render\" content=\"1\"><meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; script-src 'none'; img-src data:; style-src 'unsafe-inline'; font-src data:; base-uri 'none'; form-action 'none'\">{body}"
    ))
}

fn math_placeholder(tex: &str, display: bool) -> pulldown_cmark::Event<'static> {
    let mut escaped = String::new();
    pulldown_cmark::html::push_html(
        &mut escaped,
        [pulldown_cmark::Event::Text(tex.into())].into_iter(),
    );
    let mode = if display { "display" } else { "inline" };
    pulldown_cmark::Event::Html(
        format!("<span data-ternilo-math=\"{mode}\">{escaped}</span>").into(),
    )
}

impl ToolHandler for DocumentTool {
    fn execute<'a>(
        &'a self,
        context: ToolExecutionContext,
        arguments: Value,
    ) -> Pin<Box<dyn Future<Output = Result<ToolOutput, HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            let result = if self.review {
                let arguments: ReviewArguments = serde_json::from_value(arguments)
                    .map_err(|error| HarnessError::invalid(error.to_string()))?;
                self.review(&context, arguments).await?
            } else {
                let arguments: RenderArguments = serde_json::from_value(arguments)
                    .map_err(|error| HarnessError::invalid(error.to_string()))?;
                self.render(&context, arguments).await?
            };
            Ok(ToolOutput {
                content: serde_json::to_string(&result)
                    .map_err(|error| HarnessError::execution(error.to_string()))?,
                is_error: false,
            })
        })
    }
}
impl DocumentTool {
    async fn render(
        &self,
        context: &ToolExecutionContext,
        arguments: RenderArguments,
    ) -> Result<Value, HarnessError> {
        if !(320..=1920).contains(&arguments.width) || !(480..=1600).contains(&arguments.height) {
            return Err(HarnessError::invalid(
                "document viewport must be 320–1920 pixels wide and 480–1600 pixels high",
            ));
        }
        let workspace = context
            .workspace
            .as_ref()
            .ok_or_else(|| HarnessError::invalid("document rendering requires a workspace"))?;
        let source = read_document(&self.files, &arguments.path).await?;
        let html = render_html(&arguments.path, &source)?;
        let rendered = crate::document_browser::render(
            &self.sandboxes,
            &context.cancellation,
            Path::new(&workspace.path),
            &self.config.browser_command,
            html,
            arguments.width,
            arguments.height,
        )
        .await?;
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| HarnessError::execution(error.to_string()))?
            .as_nanos();
        let render_id = digest(&format!("{}:{}:{stamp}", context.run_id, context.call_id));
        let mut screenshots = Vec::new();
        for (index, content) in rendered.screenshots.into_iter().enumerate() {
            screenshots.push(
                self.attachments
                    .store(Attachment {
                        name: format!(
                            "{}-page-{}.png",
                            Path::new(&arguments.path)
                                .file_stem()
                                .and_then(|name| name.to_str())
                                .unwrap_or("document"),
                            index + 1
                        ),
                        media_type: "image/png".into(),
                        content,
                    })
                    .await?,
            );
        }
        let render = DocumentRender {
            render_id,
            path: arguments.path,
            source_sha256: digest(&source),
            viewport_width: arguments.width,
            viewport_height: arguments.height,
            content_height: rendered.height,
            horizontal_overflow: rendered.overflow,
            missing_images: rendered.missing_images,
            blocked_resources: rendered.blocked_resources,
            screenshots,
        };
        let output = json!({"render_id":render.render_id,"path":render.path,"source_sha256":render.source_sha256,"viewport":{"width":render.viewport_width,"height":render.viewport_height},"content_height":render.content_height,"pages":render.screenshots.len(),"horizontal_overflow":render.horizontal_overflow,"missing_images":render.missing_images,"blocked_resources":render.blocked_resources,"visual_review":"pending"});
        self.sessions
            .append(
                context.run_id.clone(),
                SessionEventKind::DocumentRendered { render },
            )
            .await?;
        Ok(output)
    }
    async fn review(
        &self,
        context: &ToolExecutionContext,
        arguments: ReviewArguments,
    ) -> Result<Value, HarnessError> {
        if arguments.findings.is_empty()
            || arguments.findings.len() > 20
            || arguments
                .findings
                .iter()
                .any(|finding| finding.trim().is_empty() || finding.chars().count() > 1000)
        {
            return Err(HarnessError::invalid(
                "document review requires 1–20 concrete findings of at most 1000 characters each",
            ));
        }
        let events = self.sessions.events().await;
        let render = events
            .iter()
            .rev()
            .find_map(|event| match &event.kind {
                SessionEventKind::DocumentRendered { render }
                    if render.render_id == arguments.render_id =>
                {
                    Some(render)
                }
                _ => None,
            })
            .ok_or_else(|| {
                HarnessError::invalid("document render is not present in this session")
            })?;
        let latest = events.iter().rev().find_map(|event| match &event.kind {
            SessionEventKind::DocumentRendered { render: candidate }
                if candidate.path == render.path =>
            {
                Some(candidate)
            }
            _ => None,
        });
        if latest.is_some_and(|latest| latest.render_id != render.render_id)
            || digest(&read_document(&self.files, &render.path).await?) != render.source_sha256
        {
            return Err(HarnessError::conflict(
                "document changed or was rendered again; review a new rendering of the current source",
            ));
        }
        if arguments.verdict == DocumentVerdict::Pass
            && (render.horizontal_overflow
                || render.missing_images > 0
                || render.blocked_resources > 0)
        {
            return Err(HarnessError::invalid(
                "document still has overflow, missing images or blocked resources; fix the source and render again before passing review",
            ));
        }
        let review = DocumentReview {
            render_id: render.render_id.clone(),
            path: render.path.clone(),
            source_sha256: render.source_sha256.clone(),
            verdict: arguments.verdict,
            findings: arguments.findings,
        };
        self.sessions
            .append(
                context.run_id.clone(),
                SessionEventKind::DocumentReviewed {
                    review: review.clone(),
                },
            )
            .await?;
        serde_json::to_value(review).map_err(|error| HarnessError::execution(error.to_string()))
    }
}

pub(crate) fn context_message(kind: &SessionEventKind) -> Option<ternilo_protocol::ModelMessage> {
    use ternilo_protocol::{MessageRole, ModelMessage};
    match kind {
        SessionEventKind::DocumentRendered { render } => Some(ModelMessage {
            role: MessageRole::User,
            content: format!(
                "<document_render id={:?} path={:?} source_sha256={:?}>Inspect every attached viewport screenshot before recording a review. Overflow: {}; missing images: {}; blocked resources: {}.</document_render>",
                render.render_id,
                render.path,
                render.source_sha256,
                render.horizontal_overflow,
                render.missing_images,
                render.blocked_resources
            ),
            reasoning_content: None,
            provider_state: None,
            attachments: render.screenshots.clone(),
            tool_call_id: None,
            tool_calls: Vec::new(),
        }),
        SessionEventKind::DocumentReviewed { review } => Some(ModelMessage {
            role: MessageRole::User,
            content: format!(
                "<document_review>{}</document_review>",
                serde_json::to_string(review).unwrap_or_default()
            ),
            reasoning_content: None,
            provider_state: None,
            attachments: Vec::new(),
            tool_call_id: None,
            tool_calls: Vec::new(),
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::render_html;

    #[test]
    fn markdown_math_escapes_source_and_preserves_code_and_currency_literals() {
        let html = render_html(
            "guide.md",
            r"Inline $x < y$ and $$\frac{1}{2}$$.
`$literal$` and \$20.
```tex
$not_math$
```",
        )
        .unwrap();
        assert_eq!(html.matches("data-ternilo-math=\"").count(), 2);
        assert!(html.contains("x &lt; y"));
        assert!(html.contains("<code>$literal$</code>"));
        assert!(html.contains("$not_math$"));
        assert!(html.contains("$20"));
        assert!(html.contains("script-src 'none'"));
    }
}
