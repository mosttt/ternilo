use serde_json::{Value, json};

/// Keep reusable instructions ahead of identity and discovery details.
/// Callers opt in only on protocols that understand explicit cache breakpoints.
pub(crate) fn anthropic_system(system: &str, enabled: bool) -> Value {
    if !enabled {
        return json!(system);
    }
    let boundary = system.rfind("\n\nSession identity:");
    match boundary.filter(|index| *index > 0) {
        Some(index) => json!([
            {"type":"text","text":&system[..index],"cache_control":{"type":"ephemeral"}},
            {"type":"text","text":&system[index..]},
        ]),
        None => json!([{"type":"text","text":system,"cache_control":{"type":"ephemeral"}}]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cache_breakpoint_keeps_identity_out_of_the_shared_prefix_and_preserves_all_text() {
        let a = "Stable instructions\n\nProject conventions\n\nSession identity: session=a\n\nTools loaded: read";
        let b = "Stable instructions\n\nProject conventions\n\nSession identity: session=b\n\nTools loaded: read, search";
        let cached_a = anthropic_system(a, true);
        let cached_b = anthropic_system(b, true);
        assert_eq!(cached_a[0], cached_b[0]);
        assert_ne!(cached_a[1], cached_b[1]);
        assert_eq!(
            format!(
                "{}{}",
                cached_a[0]["text"].as_str().unwrap(),
                cached_a[1]["text"].as_str().unwrap()
            ),
            a
        );
        assert_eq!(anthropic_system(a, false), json!(a));
        assert_eq!(
            anthropic_system("Plain system", true)[0]["text"],
            "Plain system"
        );
    }
}
