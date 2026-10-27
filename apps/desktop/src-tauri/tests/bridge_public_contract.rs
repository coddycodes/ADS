use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::mpsc,
    time::Duration,
};

use coddy_studio_desktop_lib::{
    desktop_bridge_bootstrap_ssh_bearer_session, desktop_bridge_fetch_environment_descriptor,
    desktop_bridge_fetch_ssh_session_state, desktop_bridge_issue_ssh_web_socket_ticket,
};
use serde_json::json;

fn read_request(stream: &mut TcpStream) -> String {
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("read timeout should configure");
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 1024];
    loop {
        match stream.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => {
                bytes.extend_from_slice(&buffer[..read]);
                let text = String::from_utf8_lossy(&bytes);
                let Some(header_end) = text.find("\r\n\r\n") else {
                    continue;
                };
                let content_length = text
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().ok())
                            .flatten()
                    })
                    .unwrap_or(0);
                if bytes.len().saturating_sub(header_end + 4) >= content_length {
                    break;
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                break;
            }
            Err(error) => panic!("test server failed to read request: {error}"),
        }
    }
    String::from_utf8(bytes).expect("request should be UTF-8")
}

fn spawn_json_server(status: &'static str, body: &'static str) -> (String, mpsc::Receiver<String>) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("test server should bind");
    let address = listener.local_addr().expect("test server address");
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("test server should accept");
        sender
            .send(read_request(&mut stream))
            .expect("request should be observed");
        let response = format!(
            "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len(),
        );
        stream
            .write_all(response.as_bytes())
            .expect("test server should respond");
    });
    (format!("http://{address}"), receiver)
}

#[tokio::test]
async fn public_remote_bridge_commands_route_and_decode_environment_requests() {
    let (base_url, requests) = spawn_json_server("200 OK", r#"{"environmentId":"environment-1"}"#);
    assert_eq!(
        desktop_bridge_fetch_environment_descriptor(base_url)
            .await
            .expect("descriptor should load"),
        json!({"environmentId":"environment-1"}),
    );
    assert!(
        requests
            .recv()
            .expect("descriptor request")
            .starts_with("GET /.well-known/coddy-studio/environment HTTP/1.1")
    );

    let (base_url, requests) = spawn_json_server("200 OK", r#"{"status":"authenticated"}"#);
    assert_eq!(
        desktop_bridge_fetch_ssh_session_state(base_url, "bearer-token".to_string())
            .await
            .expect("session should load"),
        json!({"status":"authenticated"}),
    );
    assert!(
        requests
            .recv()
            .expect("session request")
            .contains("authorization: Bearer bearer-token")
    );

    let (base_url, requests) = spawn_json_server(
        "200 OK",
        r#"{"access_token":"token","token_type":"Bearer"}"#,
    );
    assert_eq!(
        desktop_bridge_bootstrap_ssh_bearer_session(base_url, "credential".to_string(), None)
            .await
            .expect("bearer session should bootstrap")["access_token"],
        "token",
    );
    let request = requests.recv().expect("bootstrap request");
    assert!(request.starts_with("POST /oauth/token HTTP/1.1"));
    assert!(request.contains("subject_token=credential"));

    let (base_url, requests) = spawn_json_server("200 OK", r#"{"ticket":"ticket-1"}"#);
    assert_eq!(
        desktop_bridge_issue_ssh_web_socket_ticket(base_url, "bearer-token".to_string())
            .await
            .expect("ticket should issue")["ticket"],
        "ticket-1",
    );
    assert!(
        requests
            .recv()
            .expect("ticket request")
            .starts_with("POST /api/auth/websocket-ticket HTTP/1.1")
    );

    for invalid_base_url in ["not a URL", "file:///tmp/blocked"] {
        assert!(
            desktop_bridge_fetch_environment_descriptor(invalid_base_url.to_string())
                .await
                .is_err()
        );
    }

    let (base_url, requests) = spawn_json_server("500 Internal Server Error", "{}");
    assert!(
        desktop_bridge_fetch_environment_descriptor(base_url)
            .await
            .expect_err("a failed remote response must be rejected")
            .contains("ssh_http:500")
    );
    requests.recv().expect("failed request");

    let (base_url, requests) = spawn_json_server("200 OK", "not-json");
    assert!(
        desktop_bridge_fetch_environment_descriptor(base_url)
            .await
            .expect_err("invalid remote JSON must be rejected")
            .contains("Could not decode")
    );
    requests.recv().expect("invalid JSON request");

    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("unused address should bind");
    let unreachable_url = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    assert!(
        desktop_bridge_fetch_environment_descriptor(unreachable_url)
            .await
            .expect_err("an unreachable remote must fail")
            .contains("Could not reach")
    );
}
