use crate::Attachment;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DocumentRender {
    pub render_id: String,
    pub path: String,
    pub source_sha256: String,
    pub viewport_width: u32,
    pub viewport_height: u32,
    pub content_height: u32,
    pub horizontal_overflow: bool,
    pub missing_images: u32,
    pub blocked_resources: u32,
    pub screenshots: Vec<Attachment>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DocumentVerdict {
    Pass,
    NeedsChanges,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DocumentReview {
    pub render_id: String,
    pub path: String,
    pub source_sha256: String,
    pub verdict: DocumentVerdict,
    pub findings: Vec<String>,
}
