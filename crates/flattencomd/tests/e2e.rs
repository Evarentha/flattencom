/*
 * flattencom - flattencom Flattencomd Tests E2E
 *
 * Exercises a real service process through authentication, sessions, capture, export and shutdown.
 *
 * Authors:
 * worryzu <worryzu@gmail.com> @LinearTeam
 *
 * Copyright (C) 2026 Evarentha
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

//! Daemon end-to-end tests launch the real flattencomd binary and use DaemonClient
//! to cover authentication, ports, sessions, IO, decoders, filters, triggers, exports and closure.
//!
//! FLATTENCOM_SOCKET and FLATTENCOM_STATE_DIR are injected into child processes
//! and point to temporary paths, keeping tests separate from user state.

#![forbid(unsafe_code)]

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

use flattencom_proto::client::{ConnectConfig, DaemonClient};
use flattencom_proto::methods::*;

fn params(value: serde_json::Value) -> serde_json::Value {
    value
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn 守护进程_端到端全流程() {
    let dir = tempfile::tempdir().expect("临时目录失败");
    #[cfg(unix)]
    let socket = dir.path().join("e2e.sock");
    #[cfg(windows)]
    let socket = PathBuf::from(format!(r"\\.\pipe\flattencom-test-{}", std::process::id()));
    let state_dir = dir.path().join("state");

    // Launch the foreground daemon with isolated endpoint environment variables.
    let mut child = Command::new(env!("CARGO_BIN_EXE_flattencomd"))
        .arg("--foreground")
        .env("FLATTENCOM_SOCKET", &socket)
        .env("FLATTENCOM_STATE_DIR", &state_dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("启动 flattencomd 失败");

    // Explicit client configuration performs the authentication handshake automatically.
    let cfg = ConnectConfig::new("e2e-test", Duration::from_secs(2))
        .with_socket(&socket)
        .with_state_dir(&state_dir);
    let client = connect_with_retry(cfg.clone()).await;
    // A second observer uses the same live serial port and independent view filter.
    let observer = connect_with_retry(cfg.clone()).await;
    // Compatibility jobs reject malformed parameter shapes promptly, without
    // losing their correlated response or consuming the connection's permits.
    for method in ["send_file", "replay_log"] {
        for value in [
            serde_json::Value::Null,
            serde_json::json!([]),
            serde_json::json!("bad"),
            serde_json::json!(true),
            serde_json::json!(42),
        ] {
            let error = client
                .call_typed::<_, serde_json::Value>(method, &value, Duration::from_secs(2))
                .await
                .unwrap_err();
            assert_eq!(error.code(), error_code::INVALID_PARAMS);
        }
    }
    // Labels are bounded in UTF-8 bytes at authenticated hello, before registration.
    let mut bounded = cfg.clone();
    bounded.label = "é".repeat(MAX_CLIENT_LABEL_BYTES / 2);
    let accepted = DaemonClient::connect_with(bounded.clone()).await.unwrap();
    accepted.close();
    bounded.label.push('x');
    let rejected = match DaemonClient::connect_with(bounded.clone()).await {
        Ok(_) => panic!("oversized label accepted"),
        Err(error) => error,
    };
    assert_eq!(rejected.code(), error_code::INVALID_PARAMS);
    #[cfg(unix)]
    {
        use tokio::io::AsyncWriteExt;
        let stream = tokio::net::UnixStream::connect(&socket).await.unwrap();
        let (read, mut write) = stream.into_split();
        let mut read = tokio::io::BufReader::new(read);
        let token = std::fs::read_to_string(state_dir.join("daemon.token")).unwrap();
        let hello = serde_json::json!({"jsonrpc":"2.0","id":1,"method":"hello","params":{"client":"raw-test","proto":PROTOCOL_VERSION,"token":token.trim()}});
        write
            .write_all(format!("{hello}\n").as_bytes())
            .await
            .unwrap();
        flattencom_proto::framing::read_line(&mut read, 65536)
            .await
            .unwrap()
            .unwrap();
        for (request, code, id) in [
            (
                r#"{"jsonrpc":"2.0","id":2}"#,
                error_code::INVALID_REQUEST,
                2,
            ),
            (
                r#"{"jsonrpc":"2.0","id":"wrong","method":"daemon_info"}"#,
                error_code::INVALID_REQUEST,
                0,
            ),
            (
                r#"{"jsonrpc":"1.0","id":3,"method":"daemon_info"}"#,
                error_code::INVALID_REQUEST,
                3,
            ),
            ("{", error_code::PARSE, 0),
        ] {
            write
                .write_all(format!("{request}\n").as_bytes())
                .await
                .unwrap();
            let line = tokio::time::timeout(
                Duration::from_secs(2),
                flattencom_proto::framing::read_line(&mut read, 65536),
            )
            .await
            .unwrap()
            .unwrap()
            .unwrap();
            let response: serde_json::Value = serde_json::from_str(&line).unwrap();
            assert_eq!(response["error"]["code"], code);
            assert_eq!(response["id"], id);
        }
        // Envelope failures must leave the authenticated connection usable.
        write
            .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":4,\"method\":\"daemon_info\"}\n")
            .await
            .unwrap();
        let line = flattencom_proto::framing::read_line(&mut read, 65536)
            .await
            .unwrap()
            .unwrap();
        let response: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(response["result"]["daemon"], "flattencomd");
    }
    assert!(rejected.to_string().contains("256"));
    // Autospawn must preserve a live daemon's rejection, rather than hiding it
    // behind another startup/retry cycle.
    let rejected = match flattencom_proto::client::connect_or_spawn_with(bounded).await {
        Ok(_) => panic!("oversized label accepted by autospawn"),
        Err(error) => error,
    };
    assert_eq!(rejected.code(), error_code::INVALID_PARAMS);
    for (language, expected) in [
        ("zh-CN", "未知方法"),
        ("en", "Unknown method"),
        ("zh-CN", "未知方法"),
    ] {
        let error = client
            .call_typed::<_, serde_json::Value>(
                "unknown_language_test",
                &serde_json::json!({"_language":language}),
                Duration::from_secs(5),
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains(expected), "{error}");
    }

    // ── daemon_info ──
    let info: DaemonInfoResult = client
        .call_typed(
            "daemon_info",
            &params(serde_json::json!({})),
            Duration::from_secs(5),
        )
        .await
        .expect("daemon_info 失败");
    assert_eq!(info.daemon, "flattencomd");
    assert_eq!(info.proto, PROTOCOL_VERSION);
    assert!(info.capabilities.iter().any(|c| c == "virtual_ports"));

    // list_ports enumerates physical ports only, excluding virtual URIs.
    let ports: ListPortsResult = client
        .call_typed(
            "list_ports",
            &params(serde_json::json!({})),
            Duration::from_secs(10),
        )
        .await
        .expect("list_ports 失败");
    // Hardware may be absent; assert only valid structure and nonempty paths for returned ports.
    assert!(ports.ports.iter().all(|p| !p.info.path.is_empty()));

    // open_session: virtual loopback
    let open: OpenSessionResult = client
        .call_typed(
            "open_session",
            &params(serde_json::json!({
                "path": "virtual://echo",
                "baud": 115_200,
                "label": "e2e-echo",
            })),
            Duration::from_secs(5),
        )
        .await
        .expect("open_session 失败");
    let sid = open.session.session_id.clone();
    assert_eq!(open.session.config.path, "virtual://echo");
    assert_eq!(open.session.label.as_deref(), Some("e2e-echo"));
    assert_eq!(open.session.owner, "e2e-test");
    assert!(!open.reused);
    // New framing values round-trip across live RPC changes. Invalid tuple
    // patches must leave the accepted configuration intact.
    for parity in ["mark", "space"] {
        let result = client
            .call(
                "configure_session",
                serde_json::json!({
                    "session_id":sid, "data_bits":"five", "parity":parity,
                    "stop_bits":"one_point_five"
                }),
            )
            .await
            .unwrap();
        assert_eq!(result["config"]["parity"], parity);
        assert_eq!(result["config"]["stop_bits"], "one_point_five");
        let error = client
            .call(
                "configure_session",
                serde_json::json!({
                    "session_id":sid, "data_bits":"eight"
                }),
            )
            .await
            .unwrap_err();
        assert_eq!(error.code(), 105);
        let sessions = client
            .call("list_sessions", serde_json::json!({}))
            .await
            .unwrap();
        assert_eq!(sessions["sessions"][0]["config"]["data_bits"], "five");
    }
    client
        .call(
            "configure_session",
            serde_json::json!({
                "session_id":sid, "data_bits":"eight", "parity":"none", "stop_bits":"one"
            }),
        )
        .await
        .unwrap();
    // UTF-8 byte limits apply before open/configure mutation, including reused ports.
    let boundary_label = "é".repeat(2048);
    let oversized_label = format!("{boundary_label}x");
    for path in ["virtual://echo", "virtual://echo?delay_ms=2"] {
        let error = client
            .call(
                "open_session",
                serde_json::json!({
                    "path":path,"label":oversized_label
                }),
            )
            .await
            .unwrap_err();
        assert_eq!(error.code(), 105);
    }
    let configured = client
        .call(
            "configure_session",
            serde_json::json!({
                "session_id":sid,"label":boundary_label
            }),
        )
        .await
        .unwrap();
    assert_eq!(configured["config"]["label"], boundary_label);
    let error = client
        .call(
            "configure_session",
            serde_json::json!({
                "session_id":sid,"label":oversized_label,"baud":9600
            }),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code(), 105);
    let sessions = client
        .call("list_sessions", serde_json::json!({}))
        .await
        .unwrap();
    assert_eq!(sessions["sessions"].as_array().unwrap().len(), 1);
    assert_eq!(sessions["sessions"][0]["label"], boundary_label);
    assert_eq!(sessions["sessions"][0]["config"]["baud"], 115_200);
    client
        .call(
            "configure_session",
            serde_json::json!({
                "session_id":sid,"label":"e2e-echo"
            }),
        )
        .await
        .unwrap();

    // An escaped error can exceed the wire limit even when its request fits.
    // Return a correlated RPC error and keep this authenticated connection usable.
    let error = client
        .call(&"\u{7f}".repeat(3 * 1024 * 1024), serde_json::json!({}))
        .await
        .unwrap_err();
    assert_eq!(error.code(), error_code::INTERNAL);
    assert!(client.is_alive());
    assert_eq!(
        client
            .call("daemon_info", serde_json::json!({}))
            .await
            .unwrap()["daemon"],
        "flattencomd"
    );
    let unused_log = dir.path().join("must-not-create.log");
    let joined: OpenSessionResult = observer.call_typed("open_session", &serde_json::json!({
        "path": "virtual://echo", "baud": 9600, "label": "do not replace", "record_rx_to": unused_log,
    }), Duration::from_secs(5)).await.unwrap();
    assert!(joined.reused);
    assert_eq!(joined.session.session_id, sid);
    assert_eq!(joined.session.config, open.session.config);
    assert!(!unused_log.exists());

    // Subscribe to frame events.
    let mut events = client.subscribe_events();
    let _: SubscribeResult = client
        .call_typed(
            "subscribe",
            &params(serde_json::json!({ "session_id": sid, "kinds": ["frames", "stats"] })),
            Duration::from_secs(5),
        )
        .await
        .expect("subscribe 失败");

    // Send data and read its echo.
    let send: SendResult = client
        .call_typed(
            "send",
            &params(serde_json::json!({ "session_id": sid, "data": "PING", "newline": "crlf" })),
            Duration::from_secs(5),
        )
        .await
        .expect("send 失败");
    assert_eq!(send.bytes_sent, 6); // PING + CRLF
    let sent: FramesPageOut = observer
        .call_typed(
            "read_sent",
            &serde_json::json!({"session_id":sid,"format":"text"}),
            Duration::from_secs(5),
        )
        .await
        .unwrap();
    assert_eq!(sent.frames.len(), 1);
    assert_eq!(sent.frames[0].seq, send.seq);
    assert_eq!(sent.frames[0].text.as_deref(), Some("PING\r\n"));
    assert!(
        sent.frames[0]
            .source
            .as_deref()
            .unwrap()
            .starts_with("e2e-test#")
    );

    // Bounded wait for TX and echoed RX frames.
    let page = wait_frames(&client, &sid, 2, Duration::from_secs(5)).await;
    let texts: Vec<String> = page.frames.iter().filter_map(|f| f.hex.clone()).collect();
    assert!(
        texts.iter().any(|h| h.contains("50 49 4E 47")),
        "应有 PING 帧:{texts:?}"
    );
    let dirs: Vec<&str> = page.frames.iter().map(|f| f.dir.as_str()).collect();
    assert!(dirs.contains(&"tx"), "应有 TX:{dirs:?}");
    assert!(dirs.contains(&"rx"), "回环应有 RX:{dirs:?}");
    assert!(page.up_to_date);
    let readable_path = dir.path().join("readable.log");
    let pair: serde_json::Value = client
        .call_typed(
            "export_readable",
            &serde_json::json!({"session_id":sid,"path":readable_path}),
            Duration::from_secs(5),
        )
        .await
        .unwrap();
    let readable = std::fs::read_to_string(&readable_path).unwrap();
    assert!(readable.contains("TX #"));
    assert!(readable.contains("PING\\r\\n"));
    assert!(readable.contains(" RX #"));
    assert!(pair.get("structured_path").is_none());
    assert!(!readable_path.with_extension("txt.jsonl").exists());

    // Incremental cursors neither duplicate nor skip frames.
    let page2: FramesPageOut = client
        .call_typed(
            "read_frames",
            &params(serde_json::json!({ "session_id": sid, "since_seq": page.next_seq, "format": "raw" })),
            Duration::from_secs(5),
        )
        .await
        .expect("read_frames 增量失败");
    assert!(page2.frames.is_empty(), "游标之后应无新帧");

    // Verify frame-event notifications.
    let got_event = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match events.recv().await {
                Ok(ev) => {
                    if let Event::Frames { session_id, .. } = &*ev
                        && session_id == &sid
                    {
                        return true;
                    }
                }
                Err(_) => return false,
            }
        }
    })
    .await;
    assert!(got_event.unwrap_or(false), "应收到 frames 事件");

    // ascii_lines decoder exposes control characters.
    let _: OkResult = client
        .call_typed(
            "set_decoder",
            &params(serde_json::json!({ "session_id": sid, "spec": { "name": "ascii_lines", "options": {} } })),
            Duration::from_secs(5),
        )
        .await
        .expect("set_decoder 失败");
    let _: SendResult = client
        .call_typed(
            "send",
            &params(serde_json::json!({ "session_id": sid, "data": "AT", "newline": "crlf" })),
            Duration::from_secs(5),
        )
        .await
        .expect("send 2 失败");
    let page = wait_frames(&client, &sid, 4, Duration::from_secs(5)).await;
    let decoded = page
        .frames
        .iter()
        .any(|f| f.decoded_text.as_deref() == Some("AT␍␊"));
    assert!(
        decoded,
        "应产出可视化解码:{}",
        serde_json::to_string(&page.frames).unwrap_or_default()
    );

    // Filter to RX only.
    let _: SetFilterResult = client
        .call_typed(
            "set_filter",
            &params(serde_json::json!({ "session_id": sid, "filter": { "direction": "rx" } })),
            Duration::from_secs(5),
        )
        .await
        .expect("set_filter 失败");
    let page: FramesPageOut = client
        .call_typed(
            "read_frames",
            &params(serde_json::json!({ "session_id": sid })),
            Duration::from_secs(5),
        )
        .await
        .expect("read_frames(过滤)失败");
    assert!(page.frames.iter().all(|f| f.dir == "rx"), "过滤后应仅 rx");
    let other: FramesPageOut = observer
        .call_typed(
            "read_frames",
            &serde_json::json!({"session_id": sid}),
            Duration::from_secs(5),
        )
        .await
        .unwrap();
    assert!(
        other.frames.iter().any(|frame| frame.dir == "tx"),
        "filter must not alter another client's view"
    );

    // Trigger automatic responses on matching frames.
    let _: SetTriggersResult = client
        .call_typed(
            "set_triggers",
            &params(serde_json::json!({ "session_id": sid, "triggers": [{
                "id": "ack",
                "direction": "rx",
                "regex": "PING",
                "actions": [{ "action": "respond", "data": "ACK\r\n" }],
                "min_interval_ms": 0,
            }]})),
            Duration::from_secs(5),
        )
        .await
        .expect("set_triggers 失败");
    // Send PING again; echoed RX matches and queues ACK.
    let _: SendResult = client
        .call_typed(
            "send",
            &params(serde_json::json!({ "session_id": sid, "data": "PING", "newline": "crlf" })),
            Duration::from_secs(5),
        )
        .await
        .expect("send(触发)失败");
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let fires: GetTriggerFiresResult = client
            .call_typed(
                "get_trigger_fires",
                &params(serde_json::json!({ "session_id": sid })),
                Duration::from_secs(5),
            )
            .await
            .expect("get_trigger_fires 失败");
        if fires
            .fires
            .iter()
            .any(|f| f.trigger_id == "ack" && f.responded)
        {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "触发器应已应答:{fires:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // Live reconfiguration
    let cfg: ConfigureSessionResult = client
        .call_typed(
            "configure_session",
            &params(serde_json::json!({ "session_id": sid, "baud": 9600, "label": "e2e-renamed" })),
            Duration::from_secs(5),
        )
        .await
        .expect("configure_session 失败");
    assert_eq!(cfg.config.baud, 9600);

    // Statistics
    let stats: GetStatsResult = client
        .call_typed(
            "get_stats",
            &params(serde_json::json!({ "session_id": sid })),
            Duration::from_secs(5),
        )
        .await
        .expect("get_stats 失败");
    assert!(stats.stats.tx_bytes >= 11, "TX 字节:{:?}", stats.stats);
    assert!(
        stats.stats.rx_bytes >= 11,
        "RX 字节(回环):{:?}",
        stats.stats
    );

    // Control lines
    let sig: SignalsResult = client
        .call_typed(
            "set_signals",
            &params(serde_json::json!({ "session_id": sid, "dtr": true, "rts": false })),
            Duration::from_secs(5),
        )
        .await
        .expect("set_signals 失败");
    assert!(sig.pins.dtr);
    assert!(!sig.pins.rts);
    assert!(sig.pins.cts, "虚拟端口 CTS 常真");

    // Export
    let export_path = dir.path().join("export.jsonl");
    let exported: ExportLogResult = client
        .call_typed(
            "export_log",
            &params(serde_json::json!({ "session_id": sid, "format": "jsonl", "path": export_path.to_string_lossy(), "all": true })),
            Duration::from_secs(10),
        )
        .await
        .expect("export_log 失败");
    assert!(exported.frames >= 2, "导出至少 2 帧");
    assert!(PathBuf::from(&exported.path).exists());

    // A synchronous compatibility replay must not block control requests on
    // the same connection, and closing its session must cancel its wait.
    let replay_path = dir.path().join("timed.jsonl");
    std::fs::write(
        &replay_path,
        concat!(
            "{\"seq\":0,\"dir\":\"tx\",\"t_us\":0,\"mono_us\":0,\"data\":\"41\"}\n",
            "{\"seq\":1,\"dir\":\"tx\",\"t_us\":1,\"mono_us\":2000000,\"data\":\"42\"}\n"
        ),
    )
    .unwrap();
    let replay_client = client.clone();
    let replay_sid = sid.clone();
    let replay = tokio::spawn(async move {
        replay_client
            .call(
                "replay_log",
                serde_json::json!({"session_id":replay_sid,"path":replay_path}),
            )
            .await
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    client
        .call_typed::<_, GetStatsResult>(
            "get_stats",
            &params(serde_json::json!({"session_id":sid})),
            Duration::from_millis(500),
        )
        .await
        .expect("control request blocked by replay");

    // List and close sessions.
    let list: ListSessionsResult = client
        .call_typed(
            "list_sessions",
            &params(serde_json::json!({})),
            Duration::from_secs(5),
        )
        .await
        .expect("list_sessions 失败");
    assert_eq!(list.sessions.len(), 1);
    let _: CloseSessionResult = client
        .call_typed(
            "close_session",
            &params(serde_json::json!({ "session_id": sid })),
            Duration::from_secs(5),
        )
        .await
        .expect("close_session 失败");
    assert!(
        tokio::time::timeout(Duration::from_secs(2), replay)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
    let err = client
        .call_typed::<_, serde_json::Value>(
            "get_stats",
            &params(serde_json::json!({ "session_id": sid })),
            Duration::from_secs(5),
        )
        .await;
    assert!(err.is_err(), "会话关闭后应报错:{err:?}");

    // shutdown requests the daemon to exit on its own.
    let _: ShutdownResult = client
        .call_typed(
            "shutdown",
            &params(serde_json::json!({})),
            Duration::from_secs(5),
        )
        .await
        .expect("shutdown 失败");
    client.close();
    let exited = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match child.try_wait() {
                Ok(Some(_)) => return true,
                Ok(None) => {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                Err(_) => return false,
            }
        }
    })
    .await;
    assert!(exited.unwrap_or(false), "后台服务应正常退出");
    assert!(!socket.exists(), "套接字文件应清理");
}

/// Connect with bounded retries during daemon startup.
async fn connect_with_retry(cfg: ConnectConfig) -> DaemonClient {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        match DaemonClient::connect_with(cfg.clone()).await {
            Ok(c) => return c,
            Err(_) => {
                assert!(std::time::Instant::now() < deadline, "连接守护进程超时");
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }
}

/// Poll read_frames until the expected count or deadline.
async fn wait_frames(
    client: &DaemonClient,
    sid: &str,
    min: usize,
    timeout: Duration,
) -> FramesPageOut {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        let page: FramesPageOut = client
            .call_typed(
                "read_frames",
                &params(serde_json::json!({ "session_id": sid, "format": "decoded" })),
                Duration::from_secs(5),
            )
            .await
            .expect("read_frames 失败");
        if page.frames.len() >= min {
            return page;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "等待 {min} 帧超时:{page:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
