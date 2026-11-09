use anyhow::Result;
use base64::Engine;
use harness_remote::{RemoteClient, RemoteHost};
use rusqlite::params;
use serde_json::json;
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn remote_command_output_arrives_before_exit_survives_reconnect_and_can_cancel() -> Result<()>
{
    let temporary = tempfile::tempdir()?;
    let workspace = temporary.path().join("workspace");
    std::fs::create_dir(&workspace)?;
    let host = RemoteHost::start(
        "127.0.0.1:0".into(),
        "wss://127.0.0.1:0/".into(),
        temporary.path().join("host"),
        workspace.clone(),
    )
    .await?;
    let device = RemoteClient::pair(&host.pairing_invitation().await?).await?;
    let client = RemoteClient::new(device)?;
    client.execute("old-output","terminal_run",json!({"command":if cfg!(windows){"[Console]::Out.WriteLine('unrelated-history')"}else{"printf unrelated-history"}}),CancellationToken::new()).await?;
    let (events, mut output) = tokio::sync::mpsc::unbounded_channel();
    let cancel = CancellationToken::new();
    let worker_client = client.clone();
    let worker_cancel = cancel.clone();
    let worker = tokio::spawn(async move {
        worker_client.execute_streaming("streamed-command","terminal_run",json!({"command":if cfg!(windows){"[IO.File]::AppendAllText((Join-Path $PWD 'invocations.txt'), 'started'); [Console]::Out.Write('stream-first'); while (!(Test-Path -LiteralPath 'resume.flag')) { Start-Sleep -Milliseconds 20 }; [Console]::Error.Write('stream-second'); Start-Sleep -Seconds 20"}else{"printf started >> invocations.txt; printf stream-first; while [ ! -f resume.flag ]; do sleep 0.02; done; printf stream-second >&2; sleep 20"},"timeout_ms":30000}),worker_cancel,events).await
    });
    let mut received = vec![];
    receive_output_marker(&mut output, &mut received, "stdout", "stream-first").await?;
    assert_eq!(
        client.operation_status("streamed-command").await?["state"],
        "dispatching",
        "preview must arrive before the process exits"
    );
    client.disconnect().await;
    // The second write cannot happen until the test has closed the transport.
    // A timer alone could let both writes be queued before disconnect under load.
    std::fs::write(workspace.join("resume.flag"), b"resume")?;
    receive_output_marker(&mut output, &mut received, "stderr", "stream-second").await?;
    cancel.cancel();
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(5), worker)
            .await??
            .is_err()
    );
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while client.operation_status("streamed-command").await.unwrap()["state"] == "dispatching" {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await?;
    while let Ok(event) = output.try_recv() {
        received.push(event);
    }
    let events = client.events_after(0).await?;
    let chunks: Vec<_> = events["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|event| {
            event["operationId"] == "streamed-command" && event["payload"]["type"] == "tool_output"
        })
        .collect();
    // Pipe reads have no write-boundary guarantee, and killing a shell may add
    // a genuine stderr diagnostic. Neither changes the exactly-once contract.
    let status = client.operation_status("streamed-command").await?;
    assert_eq!(status["state"], "cancelled");
    assert_eq!(status["result"]["output_complete"], true);
    for (stream, path_key, marker) in [
        ("stdout", "stdout_artifact", "stream-first"),
        ("stderr", "stderr_artifact", "stream-second"),
    ] {
        let journal_text = chunks
            .iter()
            .filter(|event| event["payload"]["stream"] == stream)
            .map(|event| event["payload"]["text"].as_str().unwrap())
            .collect::<String>();
        let artifact = client
            .fetch_artifact(status["result"][path_key].as_str().unwrap())
            .await?;
        let actual =
            base64::engine::general_purpose::STANDARD.decode(artifact["data"].as_str().unwrap())?;
        assert_eq!(
            journal_text.as_bytes(),
            actual.as_slice(),
            "{stream} journal output duplicated or lost bytes: {chunks:?}"
        );
        assert_eq!(
            journal_text.matches(marker).count(),
            1,
            "application output repeated in {stream}: {chunks:?}"
        );
        assert!(!journal_text.contains("unrelated-history"));
    }
    let mut previous = 0;
    for delivered in received {
        let cursor = delivered["cursor"]
            .as_u64()
            .expect("missing delivered cursor");
        assert!(
            cursor > previous,
            "client replayed/reordered output cursor {cursor} after {previous}"
        );
        previous = cursor;
        assert_eq!(delivered["operationId"], "streamed-command");
        let stored = chunks
            .iter()
            .find(|event| event["cursor"].as_u64() == Some(cursor))
            .expect("delivered event is absent from journal");
        assert_eq!(delivered["stream"], stored["payload"]["stream"]);
        assert_eq!(delivered["text"], stored["payload"]["text"]);
    }
    assert_eq!(
        std::fs::read_to_string(workspace.join("invocations.txt"))?,
        "started",
        "reconnect executed the command more than once"
    );
    let operation_events: Vec<_> = events["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|event| event["operationId"] == "streamed-command")
        .collect();
    assert_eq!(
        operation_events
            .iter()
            .filter(|event| event["state"] == "accepted")
            .count(),
        1
    );
    assert_eq!(
        operation_events
            .iter()
            .filter(|event| event["state"] == "dispatching")
            .count(),
        1
    );
    host.stop().await?;
    Ok(())
}

async fn receive_output_marker(
    output: &mut tokio::sync::mpsc::UnboundedReceiver<serde_json::Value>,
    received: &mut Vec<serde_json::Value>,
    stream: &str,
    marker: &str,
) -> Result<()> {
    tokio::time::timeout(std::time::Duration::from_secs(4), async {
        let mut text = String::new();
        loop {
            let event = output
                .recv()
                .await
                .expect("stream ended before application output marker");
            assert_eq!(event["type"], "tool_output");
            assert_eq!(event["operationId"], "streamed-command");
            if event["stream"] == stream {
                text.push_str(event["text"].as_str().unwrap());
            }
            received.push(event);
            if text.contains(marker) {
                break;
            }
        }
    })
    .await?;
    Ok(())
}

#[tokio::test]
async fn remote_stream_preview_is_bounded_while_full_artifact_remains_fetchable() -> Result<()> {
    let temporary = tempfile::tempdir()?;
    let workspace = temporary.path().join("workspace");
    std::fs::create_dir(&workspace)?;
    let host = RemoteHost::start(
        "127.0.0.1:0".into(),
        "wss://127.0.0.1:0/".into(),
        temporary.path().join("host"),
        workspace,
    )
    .await?;
    let client = RemoteClient::new(RemoteClient::pair(&host.pairing_invitation().await?).await?)?;
    let (events, mut output) = tokio::sync::mpsc::unbounded_channel();
    let result=client.execute_streaming("large-output","terminal_run",json!({"command":if cfg!(windows){"[Console]::Out.Write(('x' * 70000))"}else{"awk 'BEGIN {for(i=0;i<70000;i++)printf \"x\"}'"}}),CancellationToken::new(),events).await?;
    let mut bytes = 0usize;
    let mut truncated = false;
    while let Ok(event) = output.try_recv() {
        bytes += event["text"].as_str().unwrap().len();
        truncated |= event["truncated"] == true;
    }
    assert!(bytes > 0 && bytes <= 32 * 1024);
    assert!(truncated, "preview must explicitly identify truncation");
    let artifact = client
        .fetch_artifact(result["stdout_artifact"].as_str().unwrap())
        .await?;
    assert_eq!(artifact["bytes"], 70000);
    host.stop().await?;
    Ok(())
}

#[tokio::test]
async fn desktop_lease_is_bound_to_pairing_and_session_with_release_and_revocation() -> Result<()> {
    let temporary = tempfile::tempdir()?;
    let workspace = temporary.path().join("workspace");
    std::fs::create_dir(&workspace)?;
    let host = RemoteHost::start(
        "127.0.0.1:0".into(),
        "wss://127.0.0.1:0/".into(),
        temporary.path().join("host"),
        workspace,
    )
    .await?;
    let client1 = RemoteClient::new(RemoteClient::pair(&host.pairing_invitation().await?).await?)?;
    let pairing1 = host.devices().await?[0]["id"].as_str().unwrap().to_owned();
    let client2 = RemoteClient::new(RemoteClient::pair(&host.pairing_invitation().await?).await?)?;
    let pairing2 = host
        .devices()
        .await?
        .as_array()
        .unwrap()
        .iter()
        .find(|pairing| pairing["id"] != pairing1)
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    // A harmless unknown desktop tool distinguishes the admission boundary
    // without requesting any OS accessibility or screen-recording permission.
    let error = client1
        .execute(
            "no-lease",
            "desktop_fixture_unknown",
            json!({"__harness_session_id":"session-a"}),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("lease"));
    assert!(client1.operation_status("no-lease").await.is_err());
    client1.desktop_acquire("session-a").await?;
    client1.desktop_acquire("session-a").await?;
    client1.desktop_renew("session-a").await?;
    assert!(client1.desktop_acquire("session-b").await.is_err());
    assert!(client2.desktop_acquire("session-a").await.is_err());
    assert!(client2.desktop_renew("session-a").await.is_err());
    assert!(client2.desktop_release("session-a").await.is_err());
    let wrong = client1
        .execute(
            "wrong-session",
            "desktop_fixture_unknown",
            json!({"__harness_session_id":"session-b"}),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert!(format!("{wrong:#}").contains("does not own"));
    assert!(client1.operation_status("wrong-session").await.is_err());
    let stranger = client2
        .execute(
            "wrong-pairing",
            "desktop_fixture_unknown",
            json!({"__harness_session_id":"session-a"}),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert!(format!("{stranger:#}").contains("does not own"));
    // Correct ownership reaches the real Executor (which rejects this unknown
    // fixture tool); it is no longer blocked by lease admission.
    let admitted = client1
        .execute(
            "owned-desktop",
            "desktop_fixture_unknown",
            json!({"__harness_session_id":"session-a"}),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert!(admitted.to_string().contains("Unknown tool"));
    assert_eq!(
        client1.operation_status("owned-desktop").await?["state"],
        "failed"
    );
    client1.desktop_release("session-a").await?;
    client1.desktop_release("session-a").await?;
    // Journal replay remains available after release and never re-dispatches.
    let cached = client1
        .execute(
            "owned-desktop",
            "desktop_fixture_unknown",
            json!({"__harness_session_id":"session-a"}),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert!(cached.to_string().contains("Unknown tool"));
    assert_eq!(
        client1.events_after(0).await?["events"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|event| event["operationId"] == "owned-desktop")
            .count(),
        3
    );
    assert!(client1
        .execute(
            "owned-desktop",
            "desktop_fixture_unknown",
            json!({"__harness_session_id":"session-b"}),
            CancellationToken::new()
        )
        .await
        .is_err());
    client2.desktop_acquire("session-b").await?;
    host.revoke_device(&pairing2).await?;
    client1.desktop_acquire("session-c").await?;
    assert!(client2.desktop_acquire("session-b").await.is_err());
    host.cancel_all().await?;
    assert!(host.status().await?["desktopLease"].is_null());
    client1.desktop_acquire("final-session").await?;
    host.stop().await?;
    assert!(host.status().await?["desktopLease"].is_null());
    Ok(())
}

#[tokio::test]
async fn pinned_wss_pairing_dedup_reconnect_events_and_revocation() -> Result<()> {
    let temporary = tempfile::tempdir()?;
    let workspace = temporary.path().join("workspace");
    std::fs::create_dir(&workspace)?;
    let host = RemoteHost::start(
        "127.0.0.1:0".into(),
        "wss://127.0.0.1:0/".into(),
        temporary.path().join("host"),
        workspace.clone(),
    )
    .await?;
    let invitation = host.pairing_invitation().await?;
    let device = RemoteClient::pair(&invitation).await?;
    assert!(
        RemoteClient::pair(&invitation).await.is_err(),
        "pairing invitations must be single-use"
    );
    let client = RemoteClient::new(device.clone())?;
    let capabilities = client.capabilities().await?;
    assert_eq!(capabilities["deviceId"], device.id);
    assert_eq!(capabilities["protocolVersion"], 1);

    // A wrong certificate pin is rejected before the bearer token is sent.
    let mut wrong = device.clone();
    wrong.certificate_pin = "0".repeat(64);
    let error = RemoteClient::new(wrong)?.capabilities().await.unwrap_err();
    assert!(format!("{error:#}").contains("pin verification"));

    let args = json!({"path":"once.txt","content":"one","expected_sha256":null});
    let first = client
        .execute(
            "write-once",
            "file_write",
            args.clone(),
            CancellationToken::new(),
        )
        .await?;
    assert_eq!(std::fs::read_to_string(workspace.join("once.txt"))?, "one");
    // If a duplicate dispatched, executor compare-and-swap would reject this
    // create-only request. A replay returns the already journaled result.
    client.disconnect().await;
    let replay = client
        .execute("write-once", "file_write", args, CancellationToken::new())
        .await?;
    assert_eq!(first, replay);
    assert!(client
        .execute(
            "write-once",
            "file_write",
            json!({"path":"once.txt","content":"two"}),
            CancellationToken::new()
        )
        .await
        .is_err());
    assert_eq!(std::fs::read_to_string(workspace.join("once.txt"))?, "one");
    let events = client.events_after(0).await?;
    let states: Vec<_> = events["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["state"].as_str().unwrap())
        .collect();
    assert_eq!(states, vec!["accepted", "dispatching", "completed"]);
    assert!(client
        .events_after(events["cursor"].as_u64().unwrap())
        .await?["events"]
        .as_array()
        .unwrap()
        .is_empty());
    std::fs::write(
        temporary.path().join("host/artifacts/test-log.txt"),
        "remote output",
    )?;
    let artifact = client.fetch_artifact("test-log.txt").await?;
    assert_eq!(artifact["data"], "cmVtb3RlIG91dHB1dA==");
    assert_eq!(artifact["deviceId"], device.id);
    assert!(client
        .fetch_artifact("../host-private-key.der")
        .await
        .is_err());
    assert!(client
        .fetch_artifact(workspace.join("once.txt").to_str().unwrap())
        .await
        .is_err());

    let pairings = host.devices().await?;
    host.revoke_device(pairings[0]["id"].as_str().unwrap())
        .await?;
    assert!(
        client.capabilities().await.is_err(),
        "revocation must affect an already authenticated socket"
    );
    client.disconnect().await;
    assert!(
        client.capabilities().await.is_err(),
        "revoked credentials must not reconnect"
    );
    host.stop().await?;
    Ok(())
}

#[tokio::test]
async fn persisted_tls_tokens_and_unsettled_side_effects_survive_restart_without_replay(
) -> Result<()> {
    let temporary = tempfile::tempdir()?;
    let workspace = temporary.path().join("workspace");
    let data = temporary.path().join("host");
    std::fs::create_dir(&workspace)?;
    let host = RemoteHost::start(
        "127.0.0.1:0".into(),
        "wss://127.0.0.1:0/".into(),
        data.clone(),
        workspace.clone(),
    )
    .await?;
    let device = RemoteClient::pair(&host.pairing_invitation().await?).await?;
    let pairing = host.devices().await?[0]["id"].as_str().unwrap().to_string();
    host.stop().await?;
    // Simulate durable evidence left immediately before an executor crash. The
    // restart path must never guess whether this create operation happened.
    let args =
        json!({"path":"must-not-be-created.txt","content":"unsafe replay","expected_sha256":null});
    let canonical_workspace = workspace.canonicalize()?.to_string_lossy().to_string();
    let bytes = serde_json::to_vec(
        &json!({"workspace":canonical_workspace,"name":"file_write","args":args}),
    )?;
    let request_hash: String = Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let journal = rusqlite::Connection::open(data.join("operations.sqlite"))?;
    journal.execute("INSERT INTO operations(id,request_hash,owner,workspace,name,state,created_at,updated_at) VALUES('crash-marker',?1,?2,?3,'file_write','dispatching',1,1)",params![request_hash,pairing,canonical_workspace])?;
    drop(journal);
    let port = url::Url::parse(&device.url)?.port().unwrap();
    let restarted = RemoteHost::start(
        format!("127.0.0.1:{port}"),
        device.url.clone(),
        data,
        workspace.clone(),
    )
    .await?;
    let client = RemoteClient::new(device)?;
    assert_eq!(
        client.operation_status("crash-marker").await?["state"],
        "indeterminate"
    );
    let error = client
        .execute("crash-marker", "file_write", args, CancellationToken::new())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("indeterminate"));
    assert!(!workspace.join("must-not-be-created.txt").exists());
    assert_eq!(
        client.events_after(0).await?["events"][0]["state"],
        "indeterminate"
    );
    restarted.stop().await?;
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn long_running_remote_command_can_be_cancelled_after_disconnect() -> Result<()> {
    let temporary = tempfile::tempdir()?;
    let workspace = temporary.path().join("workspace");
    std::fs::create_dir(&workspace)?;
    let host = RemoteHost::start(
        "127.0.0.1:0".into(),
        "wss://127.0.0.1:0/".into(),
        temporary.path().join("host"),
        workspace,
    )
    .await?;
    let device = RemoteClient::pair(&host.pairing_invitation().await?).await?;
    let client = RemoteClient::new(device.clone())?;
    let task_client = client.clone();
    let work = tokio::spawn(async move {
        task_client
            .execute(
                "long-command",
                "terminal_run",
                json!({"command":"sleep 20","timeout_ms":30000}),
                CancellationToken::new(),
            )
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if client
                .operation_status("long-command")
                .await
                .is_ok_and(|value| value["state"] == "dispatching")
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        }
    })
    .await?;
    client.disconnect().await;
    let other_connection = RemoteClient::new(device)?;
    other_connection.cancel("long-command").await?;
    let result = tokio::time::timeout(std::time::Duration::from_secs(5), work).await??;
    // Executor may report a killed command as a structured result or an error;
    // either settlement must terminate the wait rather than leave it running.
    let _ = result;
    assert_ne!(
        other_connection.operation_status("long-command").await?["state"],
        "dispatching"
    );
    host.stop().await?;
    Ok(())
}
