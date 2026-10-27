#[cfg(windows)]
use std::{env, path::PathBuf, process, time::Duration};

#[cfg(windows)]
use coddy_studio_server::{ServerConfig, ServerRuntime};
#[cfg(windows)]
use futures_util::{SinkExt, StreamExt};
#[cfg(windows)]
use serde_json::{Value, json};
#[cfg(windows)]
use tokio_tungstenite::{connect_async, tungstenite::Message};

#[cfg(windows)]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = env::args_os().skip(1).map(PathBuf::from);
    let base_dir = args.next().ok_or("missing base-dir argument")?;
    let repository = args.next().ok_or("missing repository argument")?;
    let common_dir = args.next().ok_or("missing common-dir argument")?;
    let ready_file = args.next().ok_or("missing ready-file argument")?;
    let stop_file = args.next().ok_or("missing stop-file argument")?;
    if args.next().is_some() {
        return Err("unexpected extra argument".into());
    }

    let config = ServerConfig::new(&base_dir)
        .with_bind("127.0.0.1", 0)
        .with_unsafe_no_auth();
    let handle = ServerRuntime::start(config).await?;
    let address = handle.local_addr();
    let (mut socket, _) = connect_async(format!("ws://{address}/ws")).await?;
    socket
        .send(Message::Text(
            json!({
                "_tag": "Request",
                "id": "7000",
                "tag": "subscribeVcsStatus",
                "payload": { "cwd": repository },
                "headers": []
            })
            .to_string()
            .into(),
        ))
        .await?;

    loop {
        let frame = socket
            .next()
            .await
            .ok_or("status stream closed before snapshot")??;
        let Message::Text(text) = frame else {
            continue;
        };
        let message: Value = serde_json::from_str(&text)?;
        if message["_tag"] == "Chunk" && message["requestId"] == "7000" {
            socket
                .send(Message::Text(
                    json!({ "_tag": "Ack", "requestId": "7000" })
                        .to_string()
                        .into(),
                ))
                .await?;
            break;
        }
        if matches!(
            message["_tag"].as_str(),
            Some("ClientProtocolError" | "Defect" | "Exit")
        ) {
            return Err(
                format!("status subscription failed before its snapshot: {message}").into(),
            );
        }
    }

    tokio::fs::write(
        &ready_file,
        format!(
            "{}\n",
            json!({
                "pid": process::id(),
                "address": address.to_string(),
                "executable": env::current_exe()?,
                "baseDir": base_dir,
                "repository": repository,
                "commonDir": common_dir,
                "physicalRepositories": 1,
                "worktrees": 1,
                "subscribers": 1
            })
        ),
    )
    .await?;

    while !stop_file.exists() {
        tokio::select! {
            frame = socket.next() => {
                let frame = frame.ok_or("status stream closed during measurement")??;
                if let Message::Text(text) = frame {
                    let message: Value = serde_json::from_str(&text)?;
                    if message["_tag"] == "Chunk" && message["requestId"] == "7000" {
                        socket.send(Message::Text(
                            json!({ "_tag": "Ack", "requestId": "7000" }).to_string().into()
                        )).await?;
                    }
                }
            }
            () = tokio::time::sleep(Duration::from_millis(100)) => {}
        }
    }

    let _ = socket
        .send(Message::Text(
            json!({ "_tag": "Interrupt", "requestId": "7000" })
                .to_string()
                .into(),
        ))
        .await;
    handle.shutdown();
    handle.join().await?;
    Ok(())
}

#[cfg(not(windows))]
fn main() {
    eprintln!("measure_vcs_runtime_server is available only on Windows");
    std::process::exit(1);
}
