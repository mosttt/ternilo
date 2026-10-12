use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

/// Operational facts retained when an earlier conversation is compacted.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ContextHandoff {
    pub objective: String,
    #[serde(default)]
    pub constraints: Vec<String>,
    #[serde(default)]
    pub decisions: Vec<String>,
    #[serde(default)]
    pub completed: Vec<String>,
    #[serde(default)]
    pub pending: Vec<String>,
    #[serde(default)]
    pub files: Vec<String>,
    #[serde(default)]
    pub verification: Vec<String>,
    #[serde(default)]
    pub risks: Vec<String>,
}

impl ContextHandoff {
    #[must_use]
    pub fn parse(summary: &str) -> Option<Self> {
        let summary = summary.trim();
        let summary = summary
            .strip_prefix("```json\n")
            .or_else(|| summary.strip_prefix("```\n"))
            .and_then(|content| content.strip_suffix("```"))
            .unwrap_or(summary);
        let value: Self = serde_json::from_str(summary).ok()?;
        (!value.objective.trim().is_empty()).then_some(value)
    }

    #[must_use]
    pub fn render(&self) -> String {
        let mut summary = format!("## Objective\n{}\n", self.objective.trim());
        for (title, items) in [
            ("Constraints", &self.constraints),
            ("Decisions", &self.decisions),
            ("Completed", &self.completed),
            ("Pending", &self.pending),
            ("Files", &self.files),
            ("Verification", &self.verification),
            ("Risks and open questions", &self.risks),
        ] {
            if items.is_empty() {
                continue;
            }
            let _ = writeln!(summary, "\n## {title}");
            for item in items {
                let _ = writeln!(summary, "- {}", item.trim());
            }
        }
        summary
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handoff_keeps_operational_facts_and_plain_checkpoints_remain_distinct() {
        let handoff = ContextHandoff::parse(r#"{"objective":"修复 저장","constraints":["Preserve user files"],"files":["src/handler.rs"],"pending":["Run the browser check"],"verification":["Not yet verified"]}"#).unwrap();
        let stored = serde_json::to_string(&handoff).unwrap();
        assert_eq!(ContextHandoff::parse(&stored).as_ref(), Some(&handoff));
        let rendered = handoff.render();
        assert!(rendered.contains("## Pending\n- Run the browser check"));
        assert!(rendered.contains("## Verification\n- Not yet verified"));
        assert!(rendered.contains("修复 저장"));
        assert!(rendered.contains("src/handler.rs"));
        assert!(ContextHandoff::parse("Keep this older plain summary").is_none());
        assert!(ContextHandoff::parse(r#"{"objective":"task","pending":true}"#).is_none());
        assert!(
            ContextHandoff::parse(
                r#"{"objective":"task","unknown":"preserve this plain checkpoint"}"#
            )
            .is_none()
        );
    }
}
