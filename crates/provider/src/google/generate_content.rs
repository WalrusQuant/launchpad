use std::collections::BTreeMap;
use std::pin::Pin;

use async_trait::async_trait;
use futures::{Stream, StreamExt};
use lpa_protocol::{
    ModelRequest, ModelResponse, ProviderFamily, RequestContent, ResponseContent, ResponseExtra,
    ResponseMetadata, StopReason, StreamEvent, Usage,
};
use reqwest::Client;
use reqwest::header::{CONTENT_TYPE, HeaderValue};
use reqwest_eventsource::{Event, EventSource};
use serde_json::{Value, json};
use tracing::debug;

use super::GoogleRole;
use crate::{
    ModelProviderSDK, ProviderAdapter, ProviderCapabilities, ProviderError, merge_extra_body,
};

pub struct GoogleProvider {
    client: Client,
    base_url: String,
    api_key: Option<String>,
}

impl GoogleProvider {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            client: Client::new(),
            base_url: base_url.into(),
            api_key: None,
        }
    }

    pub fn with_api_key(mut self, api_key: impl Into<String>) -> Self {
        self.api_key = Some(api_key.into());
        self
    }

    fn generate_content_endpoint(&self, model: &str) -> String {
        let model = model.trim_start_matches("models/");
        let base = self.base_url.trim_end_matches('/');
        format!("{base}/v1beta/models/{model}:generateContent")
    }

    fn stream_generate_content_endpoint(&self, model: &str) -> String {
        let model = model.trim_start_matches("models/");
        let base = self.base_url.trim_end_matches('/');
        format!("{base}/v1beta/models/{model}:streamGenerateContent?alt=sse")
    }

    fn request_builder(&self, url: &str, body: &Value) -> reqwest::RequestBuilder {
        let builder = self
            .client
            .post(url)
            .header(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        let builder = if let Some(api_key) = &self.api_key {
            builder.header("x-goog-api-key", api_key)
        } else {
            builder
        };
        builder.json(body)
    }
}

#[async_trait]
impl ModelProviderSDK for GoogleProvider {
    async fn completion(&self, request: ModelRequest) -> Result<ModelResponse, ProviderError> {
        let body = build_request(&request);
        let url = self.generate_content_endpoint(&request.model);
        debug!(
            provider = "google",
            api_base = %self.base_url,
            model = %request.model,
            messages = request.messages.len(),
            tools = request.tools.as_ref().map_or(0, Vec::len),
            max_tokens = request.max_tokens,
            "sending google completion request"
        );

        let response = self
            .request_builder(&url, &body)
            .send()
            .await
            .map_err(|err| ProviderError::Other {
                message: "failed to send google request".to_string(),
                source: Some(err.into()),
            })?;

        let status = response.status();
        if !status.is_success() {
            let status_code = status.as_u16();
            let body_text = response.text().await.unwrap_or_default();
            return Err(ProviderError::from_http_status(status_code, &body_text));
        }

        let value: Value = response.json().await.map_err(|err| ProviderError::Other {
            message: "failed to decode google response".to_string(),
            source: Some(err.into()),
        })?;
        parse_response(value)
    }

    async fn completion_stream(
        &self,
        request: ModelRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamEvent, ProviderError>> + Send>>, ProviderError>
    {
        let body = build_request(&request);
        let url = self.stream_generate_content_endpoint(&request.model);
        debug!(
            provider = "google",
            api_base = %self.base_url,
            model = %request.model,
            messages = request.messages.len(),
            tools = request.tools.as_ref().map_or(0, Vec::len),
            max_tokens = request.max_tokens,
            "sending google streaming request"
        );

        let event_source = EventSource::new(self.request_builder(&url, &body)).map_err(|err| {
            ProviderError::Other {
                message: "failed to create google event source".to_string(),
                source: Some(err.into()),
            }
        })?;

        let stream = async_stream::try_stream! {
            let mut response_id = String::new();
            let mut next_index = 0usize;
            let mut text_index: Option<usize> = None;
            let mut reasoning_index: Option<usize> = None;
            let mut content_blocks: BTreeMap<usize, ResponseContent> = BTreeMap::new();
            let mut reasoning_blocks: BTreeMap<usize, String> = BTreeMap::new();
            let mut stop_reason: Option<StopReason> = None;
            let mut input_tokens = 0usize;
            let mut output_tokens = 0usize;
            let mut cache_read_tokens: Option<usize> = None;

            futures::pin_mut!(event_source);
            while let Some(event) = event_source.next().await {
                let event = event.map_err(|error| {
                    ProviderError::StreamError {
                        message: format!("google stream error for model {}: {error}", request.model),
                    }
                })?;

                match event {
                    Event::Open => {}
                    Event::Message(message) => {
                        let data: Value = serde_json::from_str(&message.data)
                            .map_err(|error| ProviderError::StreamError {
                                message: format!("failed to parse google stream payload: {error}"),
                            })?;

                        if let Some(id) = data.get("responseId").and_then(Value::as_str)
                            && response_id.is_empty()
                        {
                            response_id = id.to_string();
                        }

                        if let Some(candidates) =
                            data.get("candidates").and_then(Value::as_array)
                            && let Some(candidate) = candidates.first()
                        {
                                if let Some(finish) =
                                    candidate.get("finishReason").and_then(Value::as_str)
                                {
                                    stop_reason = Some(parse_finish_reason(finish));
                                }

                                if let Some(parts) = candidate
                                    .get("content")
                                    .and_then(|c| c.get("parts"))
                                    .and_then(Value::as_array)
                                {
                                    for part in parts {
                                        let is_thought = part
                                            .get("thought")
                                            .and_then(Value::as_bool)
                                            .unwrap_or(false);

                                        if let Some(text) =
                                            part.get("text").and_then(Value::as_str)
                                        {
                                            if is_thought {
                                                let idx = *reasoning_index.get_or_insert_with(|| {
                                                    let idx = next_index;
                                                    next_index += 1;
                                                    reasoning_blocks.insert(idx, String::new());
                                                    idx
                                                });
                                                if let Some(acc) =
                                                    reasoning_blocks.get_mut(&idx)
                                                {
                                                    acc.push_str(text);
                                                }
                                                yield StreamEvent::ReasoningStart { index: idx };
                                                yield StreamEvent::ReasoningDelta {
                                                    index: idx,
                                                    text: text.to_string(),
                                                };
                                            } else {
                                                let idx = *text_index.get_or_insert_with(|| {
                                                    let idx = next_index;
                                                    next_index += 1;
                                                    content_blocks
                                                        .insert(idx, ResponseContent::Text(String::new()));
                                                    idx
                                                });
                                                if let Some(ResponseContent::Text(acc)) =
                                                    content_blocks.get_mut(&idx)
                                                {
                                                    acc.push_str(text);
                                                }
                                                yield StreamEvent::TextDelta {
                                                    index: idx,
                                                    text: text.to_string(),
                                                };
                                            }
                                        } else if let Some(fc) = part.get("functionCall") {
                                            let name = fc
                                                .get("name")
                                                .and_then(Value::as_str)
                                                .unwrap_or_default()
                                                .to_string();
                                            let args = fc
                                                .get("args")
                                                .cloned()
                                                .unwrap_or_else(|| json!({}));
                                            let idx = next_index;
                                            next_index += 1;
                                            content_blocks.insert(
                                                idx,
                                                ResponseContent::ToolUse {
                                                    id: name.clone(),
                                                    name: name.clone(),
                                                    input: args.clone(),
                                                },
                                            );
                                            yield StreamEvent::ToolCallStart {
                                                index: idx,
                                                id: name.clone(),
                                                name,
                                                input: args,
                                            };
                                        }
                                    }
                                }
                        }

                        if let Some(meta) = data.get("usageMetadata") {
                            if let Some(input) =
                                meta.get("promptTokenCount").and_then(Value::as_u64)
                            {
                                input_tokens = input as usize;
                            }
                            if let Some(output) =
                                meta.get("candidatesTokenCount").and_then(Value::as_u64)
                            {
                                output_tokens = output as usize;
                            }
                            cache_read_tokens = meta
                                .get("cachedContentTokenCount")
                                .and_then(Value::as_u64)
                                .map(|v| v as usize);
                            yield StreamEvent::UsageDelta(Usage {
                                input_tokens,
                                output_tokens,
                                cache_creation_input_tokens: None,
                                cache_read_input_tokens: cache_read_tokens,
                            });
                        }
                    }
                }
            }

            let mut final_metadata = ResponseMetadata::default();
            for text in reasoning_blocks.values() {
                if !text.is_empty() {
                    final_metadata
                        .extras
                        .push(ResponseExtra::ReasoningText { text: text.clone() });
                }
            }

            let response = ModelResponse {
                id: response_id,
                content: content_blocks.into_values().collect(),
                stop_reason,
                usage: Usage {
                    input_tokens,
                    output_tokens,
                    cache_creation_input_tokens: None,
                    cache_read_input_tokens: cache_read_tokens,
                },
                metadata: final_metadata,
            };
            yield StreamEvent::MessageDone { response };
        };

        Ok(Box::pin(stream))
    }

    fn name(&self) -> &str {
        "google"
    }
}

#[async_trait]
impl ProviderAdapter for GoogleProvider {
    fn family(&self) -> ProviderFamily {
        ProviderFamily::google()
    }

    fn capabilities(&self, _model: &str) -> ProviderCapabilities {
        ProviderCapabilities::google()
    }
}

fn build_request(request: &ModelRequest) -> Value {
    let contents: Vec<Value> = request.messages.iter().map(build_content_message).collect();

    let mut body = json!({
        "contents": contents,
        "generationConfig": {
            "maxOutputTokens": request.max_tokens,
        }
    });

    if let Some(system) = &request.system {
        body["systemInstruction"] = json!({"parts": [{"text": system}]});
    }

    if let Some(temperature) = request.sampling.temperature {
        body["generationConfig"]["temperature"] = json!(temperature);
    }
    if let Some(top_p) = request.sampling.top_p {
        body["generationConfig"]["topP"] = json!(top_p);
    }
    if let Some(top_k) = request.sampling.top_k {
        body["generationConfig"]["topK"] = json!(top_k);
    }

    if let Some(tools) = &request.tools {
        let declarations: Vec<Value> = tools
            .iter()
            .map(|tool| {
                json!({
                    "name": tool.name,
                    "description": tool.description,
                    "parameters": tool.input_schema,
                })
            })
            .collect();
        body["tools"] = json!([{"functionDeclarations": declarations}]);
    }

    if let Some(thinking) = &request.thinking
        && let Some(budget) = build_thinking_budget(thinking)
    {
        body["generationConfig"]["thinkingConfig"] = json!({"thinkingBudget": budget});
    }

    merge_extra_body(&mut body, request.extra_body.as_ref());
    body
}

fn build_content_message(message: &lpa_protocol::RequestMessage) -> Value {
    let role = message
        .role
        .parse::<GoogleRole>()
        .unwrap_or(GoogleRole::User);
    let parts: Vec<Value> = message.content.iter().map(build_part).collect();
    json!({
        "role": role.to_string(),
        "parts": parts,
    })
}

fn build_part(block: &RequestContent) -> Value {
    match block {
        RequestContent::Text { text } => json!({"text": text}),
        RequestContent::ToolUse { name, input, .. } => {
            json!({"functionCall": {"name": name, "args": input}})
        }
        RequestContent::ToolResult {
            tool_use_id,
            content,
            is_error,
        } => {
            let response = serde_json::from_str(content).unwrap_or(json!({"result": content}));
            let mut part = json!({
                "functionResponse": {
                    "name": tool_use_id,
                    "response": response,
                }
            });
            if *is_error == Some(true)
                && let Some(resp) = part["functionResponse"]["response"].as_object_mut()
            {
                resp.insert("_error".to_string(), json!(true));
            }
            part
        }
    }
}

fn parse_response(value: Value) -> Result<ModelResponse, ProviderError> {
    let mut content = Vec::new();
    let mut metadata = ResponseMetadata::default();
    let mut stop_reason = None;

    if let Some(candidates) = value.get("candidates").and_then(Value::as_array)
        && let Some(candidate) = candidates.first()
    {
        if let Some(finish) = candidate.get("finishReason").and_then(Value::as_str) {
            stop_reason = Some(parse_finish_reason(finish));
        }

        if let Some(parts) = candidate
            .get("content")
            .and_then(|c| c.get("parts"))
            .and_then(Value::as_array)
        {
            for part in parts {
                let is_thought = part
                    .get("thought")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);

                if let Some(text) = part.get("text").and_then(Value::as_str) {
                    if is_thought {
                        if !text.is_empty() {
                            metadata.extras.push(ResponseExtra::ReasoningText {
                                text: text.to_string(),
                            });
                        }
                    } else {
                        content.push(ResponseContent::Text(text.to_string()));
                    }
                } else if let Some(fc) = part.get("functionCall") {
                    let name = fc
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    let args = fc.get("args").cloned().unwrap_or_else(|| json!({}));
                    content.push(ResponseContent::ToolUse {
                        id: name.clone(),
                        name,
                        input: args,
                    });
                }
            }
        }
    }

    let usage = parse_usage(&value);

    if let Some(provider_payload) = build_provider_specific_payload(&value) {
        metadata.extras.push(ResponseExtra::ProviderSpecific {
            provider: "google".to_string(),
            payload: provider_payload,
        });
    }

    Ok(ModelResponse {
        id: value
            .get("responseId")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        content,
        stop_reason,
        usage,
        metadata,
    })
}

fn parse_usage(value: &Value) -> Usage {
    let Some(meta) = value.get("usageMetadata") else {
        return Usage::default();
    };
    Usage {
        input_tokens: meta
            .get("promptTokenCount")
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize,
        output_tokens: meta
            .get("candidatesTokenCount")
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize,
        cache_creation_input_tokens: None,
        cache_read_input_tokens: meta
            .get("cachedContentTokenCount")
            .and_then(Value::as_u64)
            .map(|v| v as usize),
    }
}

fn parse_finish_reason(value: &str) -> StopReason {
    match value {
        "STOP" => StopReason::EndTurn,
        "MAX_TOKENS" => StopReason::MaxTokens,
        _ => StopReason::EndTurn,
    }
}

fn build_thinking_budget(level: &str) -> Option<usize> {
    match level.trim().to_ascii_lowercase().as_str() {
        "disabled" => Some(0),
        "low" => Some(2048),
        "enabled" | "medium" => Some(8192),
        "high" => Some(16384),
        "xhigh" => Some(32768),
        _ => Some(8192),
    }
}

fn build_provider_specific_payload(value: &Value) -> Option<Value> {
    let mut payload = serde_json::Map::new();
    if let Some(model_version) = value.get("modelVersion").and_then(Value::as_str) {
        payload.insert("modelVersion".to_string(), json!(model_version));
    }
    if let Some(response_id) = value.get("responseId").and_then(Value::as_str) {
        payload.insert("responseId".to_string(), json!(response_id));
    }
    if payload.is_empty() {
        None
    } else {
        Some(Value::Object(payload))
    }
}

#[cfg(test)]
mod tests {
    use lpa_protocol::{
        ModelRequest, RequestContent, RequestMessage, SamplingControls, ToolDefinition,
    };
    use pretty_assertions::assert_eq;
    use serde_json::json;

    use super::{build_request, parse_finish_reason, parse_response};
    use lpa_protocol::{ResponseContent, ResponseExtra, StopReason};

    #[test]
    fn build_request_includes_generation_config_tools_and_thinking() {
        let request = ModelRequest {
            model: "gemini-2.5-flash".to_string(),
            system: Some("You are helpful.".to_string()),
            messages: vec![
                RequestMessage {
                    role: "assistant".to_string(),
                    content: vec![
                        RequestContent::Text {
                            text: "Calling tool".to_string(),
                        },
                        RequestContent::ToolUse {
                            id: "call_1".to_string(),
                            name: "get_weather".to_string(),
                            input: json!({"city": "Boston"}),
                        },
                    ],
                },
                RequestMessage {
                    role: "user".to_string(),
                    content: vec![RequestContent::ToolResult {
                        tool_use_id: "get_weather".to_string(),
                        content: "{\"temp\":72}".to_string(),
                        is_error: Some(false),
                    }],
                },
            ],
            max_tokens: 1024,
            tools: Some(vec![ToolDefinition {
                name: "get_weather".to_string(),
                description: "Get weather by city".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": { "city": { "type": "string" } },
                    "required": ["city"]
                }),
            }]),
            sampling: SamplingControls {
                temperature: Some(0.2),
                top_p: Some(0.9),
                top_k: Some(40),
            },
            thinking: Some("medium".to_string()),
            extra_body: None,
        };

        let body = build_request(&request);

        assert_eq!(body["generationConfig"]["maxOutputTokens"], json!(1024));
        assert_eq!(body["generationConfig"]["temperature"], json!(0.2));
        assert_eq!(body["generationConfig"]["topP"], json!(0.9));
        assert_eq!(body["generationConfig"]["topK"], json!(40));
        assert_eq!(
            body["generationConfig"]["thinkingConfig"]["thinkingBudget"],
            json!(8192)
        );
        assert_eq!(
            body["systemInstruction"]["parts"][0]["text"],
            json!("You are helpful.")
        );
        assert_eq!(body["contents"][0]["role"], json!("model"));
        assert_eq!(
            body["contents"][0]["parts"][1]["functionCall"]["name"],
            json!("get_weather")
        );
        assert_eq!(
            body["contents"][1]["parts"][0]["functionResponse"]["name"],
            json!("get_weather")
        );
        assert_eq!(
            body["tools"][0]["functionDeclarations"][0]["name"],
            json!("get_weather")
        );
    }

    #[test]
    fn parse_response_extracts_text_tool_use_thinking_and_usage() {
        let response = parse_response(json!({
            "responseId": "resp_123",
            "candidates": [{
                "content": {
                    "parts": [
                        {"text": "Let me think about this...", "thought": true},
                        {"text": "Here is the answer."},
                        {"functionCall": {"name": "get_weather", "args": {"city": "Boston"}}}
                    ],
                    "role": "model"
                },
                "finishReason": "STOP",
                "index": 0
            }],
            "usageMetadata": {
                "promptTokenCount": 11,
                "candidatesTokenCount": 7,
                "totalTokenCount": 18,
                "cachedContentTokenCount": 3
            },
            "modelVersion": "gemini-2.5-flash"
        }))
        .expect("parse response");

        assert_eq!(response.id, "resp_123");
        assert_eq!(response.stop_reason, Some(StopReason::EndTurn));
        assert_eq!(response.usage.input_tokens, 11);
        assert_eq!(response.usage.output_tokens, 7);
        assert_eq!(response.usage.cache_read_input_tokens, Some(3));
        assert_eq!(response.content.len(), 2);

        assert!(matches!(
            &response.content[0],
            ResponseContent::Text(t) if t == "Here is the answer."
        ));
        if let ResponseContent::ToolUse { id, name, input } = &response.content[1] {
            assert_eq!(id, "get_weather");
            assert_eq!(name, "get_weather");
            assert_eq!(input, &json!({"city": "Boston"}));
        } else {
            panic!("expected tool use block, got {:?}", response.content[1]);
        }

        assert!(response.metadata.extras.iter().any(|extra| matches!(
            extra,
            ResponseExtra::ReasoningText { text }
            if text == "Let me think about this..."
        )));
        assert!(response.metadata.extras.iter().any(|extra| matches!(
            extra,
            ResponseExtra::ProviderSpecific { provider, .. } if provider == "google"
        )));
    }

    #[test]
    fn parse_finish_reason_maps_google_finish_codes() {
        assert_eq!(parse_finish_reason("STOP"), StopReason::EndTurn);
        assert_eq!(parse_finish_reason("MAX_TOKENS"), StopReason::MaxTokens);
        assert_eq!(parse_finish_reason("SAFETY"), StopReason::EndTurn);
        assert_eq!(parse_finish_reason("RECITATION"), StopReason::EndTurn);
    }
}
