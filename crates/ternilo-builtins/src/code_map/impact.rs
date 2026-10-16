use super::{Edge, FileMap, HarnessError, Symbol, Value, json};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

pub(super) fn analyze(
    maps: &[FileMap],
    symbol_id: &str,
    max_depth: usize,
    unreadable: &[String],
) -> Result<Value, HarnessError> {
    let symbols: BTreeMap<_, _> = maps
        .iter()
        .flat_map(|map| {
            map.symbols
                .iter()
                .map(move |symbol| (symbol.id.as_str(), (map, symbol)))
        })
        .collect();
    let &(root_file, root) = symbols.get(symbol_id).ok_or_else(|| HarnessError::invalid(
        "impact symbol was not found; read code_map for the current files and select its symbol ID",
    ))?;
    let mut declarations: BTreeMap<&str, Vec<&Symbol>> = BTreeMap::new();
    for (_, symbol) in symbols.values() {
        declarations.entry(&symbol.name).or_default().push(symbol);
    }
    let mut references: BTreeMap<&str, Vec<(&FileMap, &Edge)>> = BTreeMap::new();
    for map in maps {
        for call in &map.calls {
            if let Some(name) = &call.symbol_name {
                references.entry(name).or_default().push((map, call));
            }
        }
    }
    let mut queue = VecDeque::from([(root.name.as_str(), 0)]);
    let mut visited = BTreeSet::from([root.name.as_str()]);
    let mut callers = BTreeMap::new();
    let mut sites = Vec::new();
    let mut affected_files = BTreeSet::from([root_file.path.as_str()]);
    let mut depth_limited = false;
    while let Some((name, depth)) = queue.pop_front() {
        for &(file, call) in references.get(name).into_iter().flatten() {
            affected_files.insert(&file.path);
            sites.push(json!({"path":file.path,"line":call.line,"from":call.from,"target":call.target,"called_name":name,"depth":depth+1}));
            let Some((caller_file, caller)) = call.from.as_deref().and_then(|id| symbols.get(id))
            else {
                continue;
            };
            if caller.id != root.id {
                callers
                    .entry(caller.id.as_str())
                    .or_insert((caller_file, caller, depth + 1));
            }
            if visited.contains(caller.name.as_str()) {
                continue;
            }
            if depth + 1 < max_depth {
                visited.insert(caller.name.as_str());
                queue.push_back((caller.name.as_str(), depth + 1));
            } else if references.contains_key(caller.name.as_str()) {
                depth_limited = true;
            }
        }
    }
    let ambiguous_names = visited
        .iter()
        .filter_map(|name| {
            declarations
                .get(name)
                .filter(|definitions| definitions.len() > 1)
                .map(|definitions| json!({"name":name,"declarations":definitions}))
        })
        .collect::<Vec<_>>();
    Ok(json!({
        "root":{"path":root_file.path,"symbol":root},
        "resolution":"syntactic_candidates",
        "candidate_files":affected_files,
        "candidate_callers":callers.values().map(|(file,symbol,depth)| json!({"path":file.path,"symbol":symbol,"depth":depth})).collect::<Vec<_>>(),
        "call_sites":sites,
        "ambiguous_names":ambiguous_names,
        "depth_limited":depth_limited,
        "max_depth":max_depth,
        "unreadable_files":unreadable,
        "truncated_files":maps.iter().filter(|map| map.truncated).map(|map| &map.path).collect::<Vec<_>>(),
        "syntax_error_files":maps.iter().filter(|map| map.syntax_errors).map(|map| &map.path).collect::<Vec<_>>(),
        "unresolved_call_targets":maps.iter().flat_map(|map| &map.calls).filter(|call| call.symbol_name.is_none()).count(),
        "files_analyzed":maps.iter().map(|map| &map.path).collect::<Vec<_>>()
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code_map::{language, parse};

    #[test]
    fn impact_follows_cross_file_callers_without_treating_comments_or_strings_as_calls() {
        let (_, lang) = language("lib.py").unwrap();
        let maps = vec![
            parse("lib.py", "def changed():\n    outer()\n", &lang, false).unwrap(),
            parse("app.py", "# changed()\ntext = 'changed()'\ndef caller():\n    module.changed()\ndef outer():\n    caller()\n", &lang, false).unwrap(),
        ];
        let id = &maps[0].symbols[0].id;
        let report = analyze(&maps, id, 8, &[]).unwrap();
        assert_eq!(report["call_sites"].as_array().unwrap().len(), 3);
        assert_eq!(report["candidate_callers"].as_array().unwrap().len(), 2);
        assert_eq!(report["depth_limited"], false);
        assert_eq!(report["candidate_files"], json!(["app.py", "lib.py"]));
        let bounded = analyze(&maps, id, 1, &[]).unwrap();
        assert_eq!(bounded["candidate_callers"].as_array().unwrap().len(), 1);
        assert_eq!(bounded["depth_limited"], true);
        assert!(analyze(&maps, "old-symbol-id", 8, &[]).is_err());
    }

    #[test]
    fn impact_reports_ambiguous_declarations_dynamic_targets_and_incomplete_files() {
        let (_, lang) = language("one.ts").unwrap();
        let maps = vec![
            parse(
                "one.ts",
                "function changed() {}\nfunction caller() { changed(); callbacks[key](); }",
                &lang,
                false,
            )
            .unwrap(),
            parse(
                "two.ts",
                "function changed() {}\nfunction broken(",
                &lang,
                true,
            )
            .unwrap(),
        ];
        let report = analyze(&maps, &maps[0].symbols[0].id, 8, &["unreadable.ts".into()]).unwrap();
        assert_eq!(report["ambiguous_names"].as_array().unwrap().len(), 1);
        assert_eq!(report["unresolved_call_targets"], 1);
        assert_eq!(report["unreadable_files"], json!(["unreadable.ts"]));
        assert_eq!(report["truncated_files"], json!(["two.ts"]));
        assert_eq!(report["syntax_error_files"], json!(["two.ts"]));
    }

    #[test]
    fn impact_traces_constructor_calls() {
        let (_, lang) = language("model.ts").unwrap();
        let maps = vec![
            parse(
                "model.ts",
                "class Changed {}\nfunction create() { return new Changed(); }",
                &lang,
                false,
            )
            .unwrap(),
        ];
        let report = analyze(&maps, &maps[0].symbols[0].id, 8, &[]).unwrap();
        assert_eq!(report["candidate_callers"][0]["symbol"]["name"], "create");
        assert_eq!(report["call_sites"][0]["target"], "Changed");
    }
}
