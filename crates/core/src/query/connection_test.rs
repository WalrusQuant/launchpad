use futures::StreamExt;
use lpa_protocol::{
    ModelRequest, RequestContent, RequestMessage, ResolvedThinkingRequest, ResponseContent,
    SamplingControls, StreamEvent,
};
use lpa_provider::{ModelProviderSDK, ProviderError};

use crate::{AgentError, Model};

pub async fn test_model_connection(
    provider: &dyn ModelProviderSDK,
    model: &Model,
    prompt: &str,
) -> Result<String, AgentError> {
    let ResolvedThinkingRequest {
        request_model,
        request_thinking,
        extra_body,
        effective_reasoning_effort: _,
    } = model.resolve_thinking_selection(None);
    let request = ModelRequest {
        model: request_model,
        system: None,
        messages: vec![RequestMessage {
            role: "user".to_string(),
            content: vec![RequestContent::Text {
                text: prompt.to_string(),
            }],
        }],
        max_tokens: model.max_tokens.map_or(64, |value| value as usize),
        tools: None,
        sampling: SamplingControls {
            temperature: model.temperature,
            top_p: model.top_p,
            top_k: model.top_k.map(|value| value as u32),
        },
        thinking: request_thinking,
        extra_body,
    };
    let mut stream = provider.completion_stream(request).await?;
    let mut reply_preview = String::new();
    while let Some(event) = stream.next().await {
        match event? {
            StreamEvent::TextDelta { text, .. } => reply_preview.push_str(&text),
            StreamEvent::MessageDone { response } => {
                if reply_preview.trim().is_empty() {
                    reply_preview = response
                        .content
                        .into_iter()
                        .find_map(|content| match content {
                            ResponseContent::Text(text) => Some(text),
                            _ => None,
                        })
                        .unwrap_or_default();
                }
                break;
            }
            _ => {}
        }
    }
    let preview = reply_preview.trim();
    if preview.is_empty() {
        return Err(AgentError::Provider(ProviderError::Other {
            message: "provider validation completed without a model reply".to_string(),
            source: None,
        }));
    }
    Ok(preview.to_string())
}
