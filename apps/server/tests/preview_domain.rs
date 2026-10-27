use coddy_studio_server::preview;

use preview::{
    MAX_PENDING_OPEN_REQUESTS, OpenRequestError, PreviewManager, PreviewNavStatus,
    PreviewViewportSetting,
};

#[tokio::test]
async fn opens_a_preview_session_with_normalized_localhost_url_and_emits_opened() {
    let manager = PreviewManager::new();
    let mut events = manager.subscribe_events();

    let snapshot = manager
        .open("thread-1", Some("localhost:5173"))
        .await
        .expect("open");

    assert_eq!(snapshot.thread_id, "thread-1");
    assert!(snapshot.tab_id.starts_with("tab_"));
    assert_eq!(
        snapshot.nav_status,
        PreviewNavStatus::Loading {
            url: "http://localhost:5173/".to_owned(),
            title: String::new()
        }
    );

    let event = events.recv().await.expect("opened event");
    assert_eq!(event.event_type(), "opened");
    assert_eq!(event.tab_id(), Some(snapshot.tab_id.as_str()));
}

#[tokio::test]
async fn resize_persists_across_navigation_and_status_reports() {
    let manager = PreviewManager::new();
    let opened = manager
        .open("thread-1", Some("http://localhost:5173"))
        .await
        .expect("open");

    let resized = manager
        .resize(
            "thread-1",
            &opened.tab_id,
            PreviewViewportSetting::Freeform {
                width: 1024,
                height: 768,
            },
        )
        .await
        .expect("resize");
    assert_eq!(
        resized.viewport,
        PreviewViewportSetting::Freeform {
            width: 1024,
            height: 768
        }
    );

    let navigated = manager
        .navigate(
            "thread-1",
            &opened.tab_id,
            "http://localhost:5173/about",
            Some("About"),
        )
        .await
        .expect("navigate");
    assert_eq!(navigated.viewport, resized.viewport);

    manager
        .report_status(
            "thread-1",
            &opened.tab_id,
            PreviewNavStatus::Success {
                url: "http://localhost:5173/about".to_owned(),
                title: "About".to_owned(),
            },
            true,
            false,
        )
        .await
        .expect("report status");

    let listed = manager.list("thread-1").await;
    assert_eq!(listed.sessions.len(), 1);
    assert_eq!(listed.sessions[0].viewport, resized.viewport);
}

#[tokio::test]
async fn close_is_idempotent_for_unknown_threads() {
    let manager = PreviewManager::new();
    manager.close("thread-missing", None).await.expect("close");
    assert!(manager.list("thread-missing").await.sessions.is_empty());
}

#[tokio::test]
async fn request_open_emits_event_and_first_claim_wins() {
    let manager = PreviewManager::new();
    let mut events = manager.subscribe_events();

    let receipt = manager
        .request_open("thread-1", "localhost:5173/docs")
        .await
        .expect("request open");
    assert!(receipt.delivered, "a client is subscribed");
    let request_id = receipt.request_id;

    let event = events.recv().await.expect("openRequested event");
    assert_eq!(event.event_type(), "openRequested");
    assert_eq!(event.tab_id(), None);
    let wire = serde_json::to_value(&event).expect("event encodes");
    assert_eq!(wire["type"], "openRequested");
    assert_eq!(wire["threadId"], "thread-1");
    assert_eq!(wire["requestId"], request_id.as_str());
    assert_eq!(wire["url"], "http://localhost:5173/docs");
    assert!(
        wire["createdAt"]
            .as_str()
            .is_some_and(|value| !value.is_empty())
    );

    assert!(manager.claim_open_request(&request_id).await);
    assert!(!manager.claim_open_request(&request_id).await);
    assert!(!manager.claim_open_request("open_unknown").await);
}

#[tokio::test]
async fn existing_events_use_camel_case_keys_on_the_wire() {
    let manager = PreviewManager::new();
    let mut events = manager.subscribe_events();
    let snapshot = manager.open("thread-1", None).await.expect("open");
    manager
        .close("thread-1", Some(&snapshot.tab_id))
        .await
        .expect("close");

    for _ in 0..2 {
        let wire = serde_json::to_value(events.recv().await.expect("event")).expect("encodes");
        assert_eq!(wire["threadId"], "thread-1");
        assert_eq!(wire["tabId"], snapshot.tab_id.as_str());
        assert!(wire.get("createdAt").is_some());
        assert!(wire.get("thread_id").is_none());
    }
}

#[tokio::test]
async fn request_open_rejects_non_http_and_oversized_urls() {
    let manager = PreviewManager::new();
    assert!(
        manager
            .request_open("thread-1", "file:///etc/passwd")
            .await
            .is_err()
    );
    let long = format!("http://localhost:5173/{}", "a".repeat(2048));
    assert!(manager.request_open("thread-1", &long).await.is_err());
}

#[tokio::test(start_paused = true)]
async fn open_requests_expire_after_sixty_seconds() {
    let manager = PreviewManager::new();
    let claimed_in_time = manager
        .request_open("thread-1", "http://localhost:5173")
        .await
        .expect("request open")
        .request_id;
    let claimed_too_late = manager
        .request_open("thread-1", "http://localhost:5173")
        .await
        .expect("request open")
        .request_id;

    tokio::time::advance(std::time::Duration::from_secs(59)).await;
    assert!(manager.claim_open_request(&claimed_in_time).await);

    tokio::time::advance(std::time::Duration::from_secs(2)).await;
    assert!(!manager.claim_open_request(&claimed_too_late).await);
}

#[tokio::test]
async fn internal_subscribers_do_not_count_as_delivery() {
    let manager = PreviewManager::new();
    let internal = manager.subscribe_internal_events();
    let request = |manager: PreviewManager| async move {
        manager
            .request_open("thread-1", "http://localhost:5173")
            .await
            .expect("request open")
            .delivered
    };
    assert!(!request(manager.clone()).await, "only the server listens");
    let client = manager.subscribe_events();
    assert!(request(manager.clone()).await, "a client listens");
    drop(client);
    assert!(!request(manager.clone()).await);
    drop(internal);
    assert!(!request(manager.clone()).await, "nobody listens");
}

#[tokio::test]
async fn pending_open_requests_are_capped_per_thread() {
    let manager = PreviewManager::new();
    let mut first = None;
    for _ in 0..MAX_PENDING_OPEN_REQUESTS {
        let id = manager
            .request_open("thread-1", "http://localhost:5173")
            .await
            .expect("under the cap")
            .request_id;
        first.get_or_insert(id);
    }
    assert!(matches!(
        manager
            .request_open("thread-1", "http://localhost:5173")
            .await,
        Err(OpenRequestError::TooManyPending)
    ));
    manager
        .request_open("thread-2", "http://localhost:5173")
        .await
        .expect("another thread has its own cap");
    // Claiming one frees a slot.
    assert!(manager.claim_open_request(&first.unwrap()).await);
    manager
        .request_open("thread-1", "http://localhost:5173")
        .await
        .expect("a slot was freed");
}
