// Copyright 2025-present Snab Limited (trading as Korvo)
// SPDX-License-Identifier: Apache-2.0

use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::mpsc,
    thread,
    time::Duration,
};

use relay::{
    adapters::{AnthropicAdapter, OpenAiAdapter, ProviderAdapter},
    request::{Capability, Message, RelayRequest},
};

fn request() -> RelayRequest {
    RelayRequest {
        messages: vec![Message::user("say hello")],
        model: "test-api-model".into(),
        max_output_tokens: Some(32),
        stream: false,
        metadata: Default::default(),
    }
}

fn mock_server(response_body: &'static str) -> (String, mpsc::Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut bytes = Vec::new();
        let mut buffer = [0_u8; 4096];
        let mut target_length = None;
        loop {
            let count = stream.read(&mut buffer).unwrap();
            if count == 0 {
                break;
            }
            bytes.extend_from_slice(&buffer[..count]);
            if target_length.is_none()
                && let Some(header_end) = find_bytes(&bytes, b"\r\n\r\n")
            {
                let headers = String::from_utf8_lossy(&bytes[..header_end]);
                let content_length = headers
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().ok())
                            .flatten()
                    })
                    .unwrap_or(0);
                target_length = Some(header_end + 4 + content_length);
            }
            if target_length.is_some_and(|length| bytes.len() >= length) {
                break;
            }
        }
        sender.send(String::from_utf8(bytes).unwrap()).unwrap();
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            response_body.len(),
            response_body
        )
        .unwrap();
    });
    (format!("http://{address}/v1/messages"), receiver)
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

async fn assert_common_contract(adapter: &dyn ProviderAdapter, receiver: mpsc::Receiver<String>) {
    assert!(adapter.capabilities().contains(&Capability::Chat));
    let request = request();
    assert_eq!(
        adapter.estimate_tokens(&request).unwrap(),
        adapter.estimate_tokens(&request).unwrap()
    );

    let response = adapter.complete(&request).await.unwrap();
    assert_eq!(response.text, "hello");
    assert_eq!(response.model, "test-api-model");
    assert_eq!(response.usage.input_tokens, 11);
    assert_eq!(response.usage.output_tokens, 3);
    assert!(!response.raw.to_string().contains("hello"));

    let wire = receiver.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(wire.contains("say hello"));
    assert!(wire.contains("test-api-model"));
    assert!(wire.contains("\"stream\":false"));
}

#[tokio::test]
async fn openai_adapter_passes_shared_conformance_contract() {
    let (endpoint, receiver) = mock_server(
        r#"{
            "id":"response-id",
            "model":"test-api-model",
            "choices":[{"message":{"content":"hello"},"finish_reason":"stop"}],
            "usage":{"prompt_tokens":11,"completion_tokens":3}
        }"#,
    );
    let adapter =
        OpenAiAdapter::with_endpoint("secret".into(), Duration::from_secs(5), endpoint).unwrap();
    assert_eq!(adapter.name(), "openai");
    assert_common_contract(&adapter, receiver).await;
}

#[tokio::test]
async fn anthropic_adapter_passes_shared_conformance_contract() {
    let (endpoint, receiver) = mock_server(
        r#"{
            "id":"response-id",
            "model":"test-api-model",
            "content":[{"type":"text","text":"hello"}],
            "usage":{"input_tokens":11,"output_tokens":3},
            "stop_reason":"end_turn"
        }"#,
    );
    let adapter =
        AnthropicAdapter::with_endpoint("secret".into(), Duration::from_secs(5), endpoint).unwrap();
    assert_eq!(adapter.name(), "anthropic");
    assert_common_contract(&adapter, receiver).await;
}
