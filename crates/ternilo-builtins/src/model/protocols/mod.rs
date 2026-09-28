//! Protocol selection without coupling provider configuration to plugin installation.

pub(super) mod anthropic;
pub(super) mod deepseek_responses;
pub(super) mod gemini;
mod native;
pub(super) mod openai_chat;
pub(super) mod openai_responses;
pub(super) mod responses_wire;

use super::{ProviderModel, StreamCompletion, StreamEvent};
use serde_json::Value;
use ternilo_protocol::{HarnessError, ModelRequest, ModelResponse, ProviderProtocol};

pub(super) fn request_body(
    model: &ProviderModel,
    request: &ModelRequest,
) -> Result<Value, HarnessError> {
    Ok(match model.protocol {
        ProviderProtocol::OpenAiChatCompletions => openai_chat::request_body(model, request),
        ProviderProtocol::OpenAiResponses => openai_responses::request_body(model, request),
        ProviderProtocol::DeepSeekResponses => deepseek_responses::request_body(model, request),
        ProviderProtocol::GoogleGemini => return gemini::request_body(model, request),
        ProviderProtocol::AnthropicMessages => return anthropic::request_body(model, request),
    })
}

pub(super) fn decode_stream_data(
    value: Value,
    protocol: ProviderProtocol,
    completion: &mut StreamCompletion,
) -> Result<StreamEvent, HarnessError> {
    match protocol {
        ProviderProtocol::OpenAiChatCompletions => {
            openai_chat::decode_chat_stream_data(value, completion)
        }
        ProviderProtocol::OpenAiResponses => {
            openai_responses::decode_responses_stream_data(&value, completion)
        }
        ProviderProtocol::DeepSeekResponses => {
            deepseek_responses::decode_stream_data(&value, completion)
        }
        ProviderProtocol::GoogleGemini => gemini::decode_stream_data(&value, completion),
        ProviderProtocol::AnthropicMessages => anthropic::decode_stream_data(&value, completion),
    }
}

pub(super) fn decode_completion(
    bytes: &[u8],
    protocol: ProviderProtocol,
    provider: &str,
    model: &str,
) -> Result<ModelResponse, HarnessError> {
    match protocol {
        ProviderProtocol::OpenAiChatCompletions => {
            openai_chat::decode_chat_completion(bytes, provider, model)
        }
        ProviderProtocol::OpenAiResponses => {
            openai_responses::decode_responses_completion(bytes, provider, model)
        }
        ProviderProtocol::DeepSeekResponses => {
            deepseek_responses::decode_completion(bytes, provider, model)
        }
        ProviderProtocol::GoogleGemini => gemini::decode_completion(bytes, provider, model),
        ProviderProtocol::AnthropicMessages => anthropic::decode_completion(bytes, provider, model),
    }
}
