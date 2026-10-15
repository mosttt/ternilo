use crate::{MessageRole, ModelRequest};
use serde::{Deserialize, Serialize};

/// Unicode character counts from the prepared host request, before provider encoding.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelContextUsage {
    pub system_chars: u64,
    pub tool_schema_chars: u64,
    pub conversation_chars: u64,
    pub tool_output_chars: u64,
    pub additional_state_chars: u64,
    pub unmeasured_attachments: u64,
}

fn chars(value: &str) -> u64 {
    value.chars().count() as u64
}

impl ModelRequest {
    #[must_use]
    pub fn context_usage(&self) -> ModelContextUsage {
        let mut usage = ModelContextUsage {
            system_chars: chars(&self.system_prompt),
            ..ModelContextUsage::default()
        };
        for tool in &self.tools {
            usage.tool_schema_chars += chars(&tool.name)
                + chars(&tool.description)
                + chars(&tool.input_schema.to_string());
        }
        for message in &self.messages {
            let mut content = chars(&message.content);
            for call in &message.tool_calls {
                content += chars(&call.name) + chars(&call.arguments.to_string());
            }
            for attachment in &message.attachments {
                if attachment.is_text() && !attachment.is_reference() {
                    content += chars(&attachment.content);
                } else {
                    usage.unmeasured_attachments += 1;
                }
            }
            if message.role == MessageRole::Tool {
                usage.tool_output_chars += content;
            } else {
                usage.conversation_chars += content;
            }
            usage.additional_state_chars += message.reasoning_content.as_deref().map_or(0, chars);
            if let Some(state) = &message.provider_state {
                for block in &state.blocks {
                    usage.additional_state_chars += chars(&block.to_string());
                }
            }
        }
        usage
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Attachment, ModelMessage, RunId, ToolSpec};
    use serde_json::json;

    #[test]
    fn prepared_context_counts_unicode_and_excludes_attachment_references() {
        let request = ModelRequest {
            reasoning_recommendation: None,
            prompt_cache: false,
            run_id: RunId::new("context"),
            system_prompt: "中文🙂".into(),
            tools: vec![ToolSpec {
                name: "read".into(),
                description: "读取".into(),
                input_schema: json!({}),
            }],
            messages: vec![
                ModelMessage {
                    role: MessageRole::User,
                    content: "질문".into(),
                    reasoning_content: None,
                    provider_state: None,
                    tool_calls: vec![],
                    tool_call_id: None,
                    attachments: vec![
                        Attachment {
                            name: "note.txt".into(),
                            media_type: "text/plain".into(),
                            content: "facts".into(),
                        },
                        Attachment {
                            name: "photo.png".into(),
                            media_type: "image/png".into(),
                            content: "data:image/png;base64,AAAA".into(),
                        },
                        Attachment {
                            name: "saved.txt".into(),
                            media_type: "text/plain".into(),
                            content: format!(
                                "{}{}",
                                crate::ATTACHMENT_REFERENCE_PREFIX,
                                "a".repeat(64)
                            ),
                        },
                    ],
                },
                ModelMessage {
                    role: MessageRole::Tool,
                    content: "trimmed result".into(),
                    reasoning_content: None,
                    provider_state: None,
                    attachments: vec![],
                    tool_calls: vec![],
                    tool_call_id: Some("call".into()),
                },
            ],
            step: 1,
        };
        let usage = request.context_usage();
        assert_eq!(usage.system_chars, 3);
        assert_eq!(usage.tool_schema_chars, 8);
        assert_eq!(usage.conversation_chars, 7);
        assert_eq!(usage.tool_output_chars, 14);
        assert_eq!(usage.unmeasured_attachments, 2);
        assert_eq!(usage.additional_state_chars, 0);
    }
}
