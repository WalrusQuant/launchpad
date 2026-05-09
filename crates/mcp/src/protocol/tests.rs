use pretty_assertions::assert_eq;
use serde_json::{Value, json};

use super::errors::ProtocolParseError;
use super::jsonrpc::{
    IncomingMessage, RequestId, encode_notification, encode_request, parse_message,
};
use super::messages::{CallToolResult, ContentBlock, ListToolsResult};

#[test]
fn parses_response_with_string_id() {
    let frame = br#"{"jsonrpc":"2.0","id":"abc","result":{"ok":true}}"#;
    let parsed = parse_message(frame).expect("parse");
    match parsed {
        IncomingMessage::Response(resp) => {
            assert_eq!(resp.id, RequestId::String("abc".into()));
            assert_eq!(resp.result, Some(json!({"ok": true})));
            assert!(resp.error.is_none());
        }
        other => panic!("expected Response, got {other:?}"),
    }
}

#[test]
fn parses_response_with_numeric_id() {
    let frame = br#"{"jsonrpc":"2.0","id":42,"result":null}"#;
    let parsed = parse_message(frame).expect("parse");
    match parsed {
        IncomingMessage::Response(resp) => {
            assert_eq!(resp.id, RequestId::Number(42));
        }
        other => panic!("expected Response, got {other:?}"),
    }
}

#[test]
fn parses_notification_no_id() {
    let frame = br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
    let parsed = parse_message(frame).expect("parse");
    match parsed {
        IncomingMessage::Notification(n) => {
            assert_eq!(n.method, "notifications/initialized");
            assert!(n.params.is_none());
        }
        other => panic!("expected Notification, got {other:?}"),
    }
}

#[test]
fn rejects_wrong_jsonrpc_version() {
    let frame = br#"{"jsonrpc":"1.0","id":1,"result":{}}"#;
    let err = parse_message(frame).expect_err("should reject");
    match err {
        ProtocolParseError::UnsupportedVersion { got } => assert_eq!(got, "1.0"),
        other => panic!("expected UnsupportedVersion, got {other:?}"),
    }
}

#[test]
fn tools_list_result_deserializes() {
    let raw = br#"{
        "tools": [
            {
                "name": "search",
                "description": "Search docs",
                "inputSchema": {"type":"object","properties":{"q":{"type":"string"}}}
            }
        ]
    }"#;
    let result: ListToolsResult = serde_json::from_slice(raw).expect("parse");
    assert_eq!(result.tools.len(), 1);
    assert_eq!(result.tools[0].name, "search");
    assert_eq!(result.tools[0].description, "Search docs");
}

#[test]
fn call_tool_result_with_multiple_content_blocks() {
    let raw = br#"{
        "content": [
            {"type":"text","text":"hello"},
            {"type":"image","data":"QUJD","mimeType":"image/png"}
        ],
        "isError": false
    }"#;
    let result: CallToolResult = serde_json::from_slice(raw).expect("parse");
    assert_eq!(result.content.len(), 2);
    assert!(!result.is_error);
    match &result.content[0] {
        ContentBlock::Text { text } => assert_eq!(text, "hello"),
        other => panic!("expected text block, got {other:?}"),
    }
    match &result.content[1] {
        ContentBlock::Image { mime_type, data } => {
            assert_eq!(mime_type, "image/png");
            assert_eq!(data, "QUJD");
        }
        other => panic!("expected image block, got {other:?}"),
    }
}

#[test]
fn call_tool_result_is_error_true_maps_cleanly() {
    let raw = br#"{
        "content": [{"type":"text","text":"boom"}],
        "isError": true
    }"#;
    let result: CallToolResult = serde_json::from_slice(raw).expect("parse");
    assert!(result.is_error);
}

#[test]
fn encode_request_produces_canonical_json() {
    let id = RequestId::Number(7);
    let params = json!({"name": "search", "arguments": {"q": "rust"}});
    let bytes = encode_request(&id, "tools/call", Some(&params)).expect("encode");
    let value: Value = serde_json::from_slice(&bytes).expect("parse");
    assert_eq!(
        value,
        json!({
            "jsonrpc": "2.0",
            "id": 7,
            "method": "tools/call",
            "params": {"name": "search", "arguments": {"q": "rust"}},
        })
    );
}

#[test]
fn encode_notification_without_params() {
    let bytes = encode_notification::<()>("notifications/initialized", None).expect("encode");
    let value: Value = serde_json::from_slice(&bytes).expect("parse");
    assert_eq!(
        value,
        json!({"jsonrpc":"2.0","method":"notifications/initialized"})
    );
}
