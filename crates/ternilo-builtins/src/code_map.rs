use linorun_core::{Activation, CleanupError, ComponentContext, ComponentDescriptor, effect};
use linorun_macros::component_descriptor;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{future::Future, path::Path, pin::Pin, sync::Arc};
use ternilo_kernel::{
    HarnessPlugin, PluginFactory, PluginManifest, ToolEffect, ToolExecutionContext, ToolHandler,
    ToolRegistration, Tools, WorkspaceFiles, WorkspaceFilesClient,
};
use ternilo_protocol::{FileReadRequest, HarnessError, ToolOutput, ToolSpec};

pub const KIND: &str = "ternilo.tools.code_map";
component_descriptor! {
    static DESCRIPTOR: () {
        id: "ternilo/builtin-code-map@1",
        requires: [Tools, WorkspaceFiles],
        provides: [],
    }
}
#[derive(Clone, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct Config {
    #[serde(default = "default_files")]
    max_files: usize,
    #[serde(default = "default_bytes")]
    max_file_bytes: usize,
}
const fn default_files() -> usize {
    16
}
const fn default_bytes() -> usize {
    256 * 1024
}
pub fn factory() -> PluginFactory {
    crate::factory(PluginManifest { kind: KIND, requires: &["ternilo/tools@1", "ternilo/workspace-files@2"], provides: &[] }, |value| {
        let config: Config = crate::parse_config(value)?;
        if !(1..=32).contains(&config.max_files) || !(1024..=1_048_576).contains(&config.max_file_bytes) {
            return Err(HarnessError::invalid("code map requires max_files between 1 and 32 and max_file_bytes between 1024 and 1048576"));
        }
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
            .expect("code map declares tools");
        let files = context
            .context()
            .service::<WorkspaceFiles>()
            .expect("code map declares files");
        let handler = Arc::new(CodeMap {
            files,
            config: self.config.clone(),
        });
        Activation::Once(Box::pin(async move {
            let registration = tools.register_tool(ToolRegistration {
                spec: ToolSpec {
                    name: "code_map".into(),
                    description: "Parse selected Rust, TypeScript/TSX, JavaScript/JSX, Python or Go files into AST symbols, imports and syntactic call edges with source locations. Paths must be explicit workspace files. Does not execute code or perform type resolution; dynamic calls remain unresolved. Read source around a symbol before editing.".into(),
                    input_schema: json!({"type":"object","required":["paths"],"properties":{"paths":{"type":"array","items":{"type":"string"},"minItems":1,"maxItems":32}},"additionalProperties":false}),
                }, effect: ToolEffect::ReadOnly, handler,
            }).await.map_err(|error| linorun_core::ActivationFailure::user(error.to_string()))?;
            Ok(Some(effect::inverse(move || async move {
                tools
                    .unregister_tool(registration)
                    .await
                    .map_err(|error| CleanupError::user(error.to_string()))
            })))
        }))
    }
}
struct CodeMap {
    files: WorkspaceFilesClient,
    config: Config,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Arguments {
    paths: Vec<String>,
}
impl ToolHandler for CodeMap {
    fn execute<'a>(
        &'a self,
        context: ToolExecutionContext,
        arguments: Value,
    ) -> Pin<Box<dyn Future<Output = Result<ToolOutput, HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            let args: Arguments = serde_json::from_value(arguments).map_err(|error| {
                HarnessError::invalid(format!("invalid code map arguments: {error}"))
            })?;
            if args.paths.is_empty() || args.paths.len() > self.config.max_files {
                return Err(HarnessError::invalid(format!(
                    "code map accepts 1–{} files",
                    self.config.max_files
                )));
            }
            let mut results = Vec::new();
            for path in args.paths {
                context.cancellation.check()?;
                let result = self.inspect(&path).await;
                results.push(match result {
                    Ok(map) => serde_json::to_value(map)
                        .map_err(|error| HarnessError::execution(error.to_string()))?,
                    Err(error) => json!({"path":path,"error":error.to_string()}),
                });
            }
            Ok(ToolOutput {
                content: serde_json::to_string(
                    &json!({"files":results,"edge_resolution":"syntactic"}),
                )
                .map_err(|error| HarnessError::execution(error.to_string()))?,
                is_error: false,
            })
        })
    }
}
impl CodeMap {
    async fn inspect(&self, path: &str) -> Result<FileMap, HarnessError> {
        let (_, language) = language(path)?;
        let mut source = String::new();
        let mut next = 1;
        let truncated = loop {
            let part = self
                .files
                .read_text(FileReadRequest {
                    path: path.into(),
                    start_line: Some(next),
                    line_count: Some(2000),
                })
                .await?;
            if !source.is_empty() {
                source.push('\n');
            }
            source.push_str(&part.content);
            if source.len() > self.config.max_file_bytes {
                let mut end = self.config.max_file_bytes;
                while !source.is_char_boundary(end) {
                    end -= 1;
                }
                source.truncate(end);
                break true;
            }
            if part.end_line >= part.total_lines || part.end_line < next {
                break false;
            }
            next = part.end_line + 1;
        };
        let path = path.to_owned();
        tokio::task::spawn_blocking(move || parse(&path, &source, &language, truncated))
            .await
            .map_err(|error| HarnessError::execution(format!("parse code map: {error}")))?
    }
}
#[derive(Serialize)]
struct Symbol {
    id: String,
    name: String,
    kind: String,
    line: usize,
    end_line: usize,
    parent: Option<String>,
}
#[derive(Serialize)]
struct Edge {
    from: Option<String>,
    target: String,
    line: usize,
}
#[derive(Serialize)]
struct FileMap {
    path: String,
    language: String,
    symbols: Vec<Symbol>,
    imports: Vec<Edge>,
    calls: Vec<Edge>,
    syntax_errors: bool,
    truncated: bool,
}
fn language(path: &str) -> Result<(&'static str, tree_sitter::Language), HarnessError> {
    let extension = Path::new(path)
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default();
    Ok(match extension {
        "rs" => ("rust", tree_sitter_rust::LANGUAGE.into()),
        "ts" | "mts" | "cts" => (
            "typescript",
            tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        ),
        "tsx" => ("tsx", tree_sitter_typescript::LANGUAGE_TSX.into()),
        "js" | "jsx" | "mjs" | "cjs" => ("javascript", tree_sitter_javascript::LANGUAGE.into()),
        "py" | "pyi" => ("python", tree_sitter_python::LANGUAGE.into()),
        "go" => ("go", tree_sitter_go::LANGUAGE.into()),
        _ => {
            return Err(HarnessError::invalid(
                "code map supports Rust, TypeScript, JavaScript, Python and Go files",
            ));
        }
    })
}
fn text(node: tree_sitter::Node<'_>, source: &str) -> String {
    source[node.byte_range()].chars().take(240).collect()
}
fn parse(
    path: &str,
    source: &str,
    language: &tree_sitter::Language,
    truncated: bool,
) -> Result<FileMap, HarnessError> {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(language)
        .map_err(|error| HarnessError::execution(error.to_string()))?;
    let tree = parser
        .parse(source, None)
        .ok_or_else(|| HarnessError::execution("code map parse was interrupted"))?;
    let mut map = FileMap {
        path: path.into(),
        language: self::language(path)?.0.into(),
        symbols: vec![],
        imports: vec![],
        calls: vec![],
        syntax_errors: tree.root_node().has_error(),
        truncated,
    };
    let mut stack = vec![(tree.root_node(), None)];
    while let Some((node, parent)) = stack.pop() {
        if map.symbols.len() + map.imports.len() + map.calls.len() >= 800 {
            map.truncated = true;
            break;
        }
        let kind = node.kind();
        let named = matches!(
            kind,
            "function_item"
                | "struct_item"
                | "enum_item"
                | "trait_item"
                | "type_item"
                | "mod_item"
                | "function_declaration"
                | "method_declaration"
                | "class_declaration"
                | "interface_declaration"
                | "type_alias_declaration"
                | "method_definition"
                | "function_definition"
                | "class_definition"
                | "type_spec"
        ) || (kind == "variable_declarator"
            && node.child_by_field_name("value").is_some_and(|value| {
                matches!(value.kind(), "arrow_function" | "function_expression")
            }));
        let mut owner = parent.clone();
        if named && let Some(name) = node.child_by_field_name("name") {
            let id = format!(
                "{path}:{}:{}",
                node.start_position().row + 1,
                node.start_position().column + 1
            );
            map.symbols.push(Symbol {
                id: id.clone(),
                name: text(name, source),
                kind: kind.into(),
                line: node.start_position().row + 1,
                end_line: node.end_position().row + 1,
                parent,
            });
            owner = Some(id);
        }
        if matches!(
            kind,
            "import_statement" | "import_from_statement" | "use_declaration" | "import_spec"
        ) {
            map.imports.push(Edge {
                from: owner.clone(),
                target: text(node.child_by_field_name("source").unwrap_or(node), source),
                line: node.start_position().row + 1,
            });
        }
        if matches!(kind, "call_expression" | "call" | "macro_invocation")
            && let Some(target) = node
                .child_by_field_name("function")
                .or_else(|| node.child_by_field_name("macro"))
        {
            map.calls.push(Edge {
                from: owner.clone(),
                target: text(target, source),
                line: node.start_position().row + 1,
            });
        }
        let mut cursor = node.walk();
        let children: Vec<_> = node.named_children(&mut cursor).collect();
        stack.extend(
            children
                .into_iter()
                .rev()
                .map(|child| (child, owner.clone())),
        );
    }
    Ok(map)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ast_maps_symbols_and_calls_without_parsing_comments_as_code() {
        for (path, source, expected) in [
            (
                "lib.rs",
                "// fn fake() {}\nfn actual() { target(); }",
                "actual",
            ),
            (
                "app.ts",
                "// function fake() {}\nexport function actual() { target(); }",
                "actual",
            ),
            (
                "app.tsx",
                "export const View = () => <button onClick={() => target()}/>;",
                "View",
            ),
            (
                "app.js",
                "class Example { actual() { target(); } }",
                "actual",
            ),
            (
                "app.py",
                "# def fake(): pass\ndef actual():\n    target()",
                "actual",
            ),
            (
                "app.go",
                "package main\nfunc actual() { target() }",
                "actual",
            ),
        ] {
            let (_, lang) = language(path).unwrap();
            let map = parse(path, source, &lang, false).unwrap();
            assert!(
                map.symbols.iter().any(|symbol| symbol.name == expected),
                "{path}"
            );
            assert!(!map.symbols.iter().any(|symbol| symbol.name == "fake"));
            assert!(
                map.calls.iter().any(|edge| edge.target == "target"),
                "{path}"
            );
            assert!(!map.syntax_errors, "{path}");
        }
        let (_, lang) = language("app.rs").unwrap();
        assert!(
            parse("app.rs", "fn unfinished(", &lang, true)
                .unwrap()
                .syntax_errors
        );
        assert!(language("binary.exe").is_err());
    }
}
