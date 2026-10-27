use coddy_studio_server::mcp::preview_automation;

use preview_automation::{
    PreviewAutomationBroker, PreviewAutomationHost, PreviewAutomationInvokeInput,
    PreviewAutomationOperation, PreviewAutomationResponse, PreviewAutomationStreamEvent,
};
use serde_json::json;

#[test]
fn operation_wire_literals_match_the_frontend_contract() {
    let values = [
        "status",
        "open",
        "navigate",
        "snapshot",
        "click",
        "type",
        "press",
        "scroll",
        "evaluate",
        "waitFor",
        "recordingStart",
        "recordingStop",
        "resize",
    ];

    for value in values {
        let operation = PreviewAutomationOperation::from_wire(value).expect("known operation");
        assert_eq!(operation.as_str(), value);
    }
    assert_eq!(PreviewAutomationOperation::from_wire("unknown"), None);
}

#[tokio::test]
async fn connect_announces_connection_before_delivering_requests() {
    let broker = PreviewAutomationBroker::new();
    let mut stream = broker.connect(PreviewAutomationHost {
        client_id: "client-1".to_owned(),
        environment_id: "environment-1".to_owned(),
        supported_operations: vec![PreviewAutomationOperation::Open],
    });

    let broker_for_task = broker.clone();
    let task = tokio::spawn(async move {
        let first = stream.recv().await.expect("connected");
        let PreviewAutomationStreamEvent::Connected { connection_id } = first else {
            panic!("expected connected event");
        };
        let second = stream.recv().await.expect("request");
        let PreviewAutomationStreamEvent::Request {
            connection_id: request_connection_id,
            request,
        } = second
        else {
            panic!("expected request event");
        };
        assert_eq!(request_connection_id, connection_id);
        broker_for_task
            .respond(PreviewAutomationResponse {
                client_id: "client-1".to_owned(),
                connection_id,
                request_id: request.request_id,
                ok: true,
                result: Some(json!({"available": true, "tabId": "tab-web"})),
                error: None,
            })
            .await
            .expect("response");
    });

    let result = broker
        .invoke(PreviewAutomationInvokeInput {
            environment_id: "environment-1".to_owned(),
            thread_id: "thread-1".to_owned(),
            provider_session_id: "provider-session-1".to_owned(),
            provider_instance_id: "codex".to_owned(),
            operation: PreviewAutomationOperation::Open,
            input: json!({"reuseExistingTab": false}),
            tab_id: None,
            timeout_ms: Some(1_000),
        })
        .await
        .expect("invoke result");

    assert_eq!(result["available"], true);
    task.await.expect("task join");
}

#[tokio::test]
async fn replacing_a_connection_fails_pending_requests_with_client_disconnected() {
    let broker = PreviewAutomationBroker::new();
    let _first = broker.connect(PreviewAutomationHost {
        client_id: "client-1".to_owned(),
        environment_id: "environment-1".to_owned(),
        supported_operations: vec![PreviewAutomationOperation::Open],
    });

    let pending = tokio::spawn({
        let broker = broker.clone();
        async move {
            broker
                .invoke(PreviewAutomationInvokeInput {
                    environment_id: "environment-1".to_owned(),
                    thread_id: "thread-1".to_owned(),
                    provider_session_id: "provider-session-1".to_owned(),
                    provider_instance_id: "codex".to_owned(),
                    operation: PreviewAutomationOperation::Open,
                    input: json!({}),
                    tab_id: None,
                    timeout_ms: Some(5_000),
                })
                .await
                .expect_err("pending request should fail")
        }
    });

    tokio::task::yield_now().await;
    let _replacement = broker.connect(PreviewAutomationHost {
        client_id: "client-1".to_owned(),
        environment_id: "environment-1".to_owned(),
        supported_operations: vec![PreviewAutomationOperation::Open],
    });

    let error = pending.await.expect("pending join");
    assert_eq!(error.tag(), "PreviewAutomationClientDisconnectedError");
}

#[tokio::test]
async fn disconnect_removes_only_the_matching_connection_and_fails_pending_requests() {
    let broker = PreviewAutomationBroker::new();
    let mut stream = broker.connect(PreviewAutomationHost {
        client_id: "client-1".to_owned(),
        environment_id: "environment-1".to_owned(),
        supported_operations: vec![PreviewAutomationOperation::Open],
    });
    let PreviewAutomationStreamEvent::Connected { connection_id } =
        stream.recv().await.expect("connected")
    else {
        panic!("expected connected event");
    };

    let pending = tokio::spawn({
        let broker = broker.clone();
        async move {
            broker
                .invoke(PreviewAutomationInvokeInput {
                    environment_id: "environment-1".to_owned(),
                    thread_id: "thread-1".to_owned(),
                    provider_session_id: "provider-session-1".to_owned(),
                    provider_instance_id: "codex".to_owned(),
                    operation: PreviewAutomationOperation::Open,
                    input: json!({}),
                    tab_id: None,
                    timeout_ms: Some(5_000),
                })
                .await
                .expect_err("pending request should fail")
        }
    });
    let _ = stream.recv().await.expect("request");

    assert!(!broker.disconnect("client-1", "wrong-connection"));
    assert!(broker.disconnect("client-1", &connection_id));
    assert_eq!(
        pending.await.expect("pending join").tag(),
        "PreviewAutomationClientDisconnectedError"
    );
}
