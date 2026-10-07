/*
 * flattencom - flattencom Flattencom-Mcp Tests E2E
 *
 * Exercises real MCP stdio processes, tool schemas, resources, translations and selection restrictions.
 *
 * Authors:
 * worryzu <worryzu@gmail.com> @LinearTeam
 *
 * Copyright (C) 2026 Evarentha
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

//! MCP end-to-end protocol tests launch real flattencomd and flattencom-mcp processes
//! and drive complete client interactions through stdio:
//! initialize, list/call tools, list/read resources, and retrieve prompts.
//!
//! Verify both protocol compliance and data correctness through the actual bridge.

#![forbid(unsafe_code)]

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

/// MCP protocol version requested by the client and negotiated with the server.
const PROTOCOL: &str = "2025-06-18";

struct McpChild {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<std::process::ChildStdout>,
    next_id: u64,
    notifications: Vec<Value>,
}

impl McpChild {
    fn send(&mut self, method: &str, params: Value) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        let msg = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        let stdin = self.stdin.as_mut().expect("stdin 已关闭");
        writeln!(stdin, "{msg}").expect("写 MCP stdin 失败");
        stdin.flush().expect("刷新 stdin 失败");
        id
    }

    fn notify(&mut self, method: &str, params: Value) {
        let msg = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        let stdin = self.stdin.as_mut().expect("stdin 已关闭");
        writeln!(stdin, "{msg}").expect("写通知失败");
        stdin.flush().expect("刷新失败");
    }

    /// Read the response with the requested ID, retaining intervening notifications.
    fn read_response(&mut self, id: u64) -> Value {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            assert!(Instant::now() < deadline, "等待响应 {id} 超时");
            let mut line = String::new();
            let n = self
                .stdout
                .read_line(&mut line)
                .expect("读 MCP stdout 失败");
            assert!(n > 0, "MCP 进程提前退出");
            if line.trim().is_empty() {
                continue;
            }
            let v: Value = serde_json::from_str(&line).expect("响应不是合法 JSON");
            if v.get("id") == Some(&Value::from(id)) {
                return v;
            }
            if v.get("method").is_some() {
                self.notifications.push(v);
            }
        }
    }

    /// Send a request and read its response.
    fn roundtrip(&mut self, method: &str, params: Value) -> Value {
        let id = self.send(method, params);
        self.read_response(id)
    }

    /// Require a response without a protocol error field.
    fn must_ok(v: Value) -> Value {
        assert!(
            v.get("error").is_none(),
            "MCP 调用报错:{}",
            serde_json::to_string_pretty(&v).unwrap_or_default()
        );
        v["result"].clone()
    }

    /// Call a tool and require success.
    fn call_tool(&mut self, name: &str, arguments: Value) -> Value {
        let r = self.roundtrip(
            "tools/call",
            serde_json::json!({ "name": name, "arguments": arguments }),
        );
        let result = Self::must_ok(r);
        assert_ne!(result["isError"], true, "tool {name} failed: {result}");
        result
    }

    fn wait_for_catalog_change(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !self
            .notifications
            .iter()
            .any(|note| note["method"] == "notifications/resources/list_changed")
        {
            assert!(
                Instant::now() < deadline,
                "missing resource catalog invalidation"
            );
            std::thread::sleep(Duration::from_millis(50));
            self.call_tool("list_sessions", serde_json::json!({}));
        }
    }
}

/// Locate the sibling flattencomd binary beside the MCP test executable.
fn daemon_path() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_BIN_EXE_flattencom-mcp"))
        .parent()
        .expect("父目录")
        .join(if cfg!(windows) {
            "flattencomd.exe"
        } else {
            "flattencomd"
        })
}

/// Compare runtime descriptions by schema path, including nested definitions and arrays.
fn assert_translated_descriptions(
    english: &Value,
    chinese: &Value,
    catalog: &Value,
    path: &str,
) -> usize {
    match english {
        Value::Object(object) => {
            let translated = chinese
                .as_object()
                .unwrap_or_else(|| panic!("missing translated object at {path}"));
            object
                .iter()
                .map(|(key, value)| {
                    let path = format!("{path}/{key}");
                    let actual = translated
                        .get(key)
                        .unwrap_or_else(|| panic!("missing translated schema entry at {path}"));
                    if key == "description"
                        && let Some(source) = value.as_str()
                    {
                        let expected = catalog[source].as_str().unwrap_or_else(|| {
                            panic!("missing Chinese catalog entry at {path}: {source:?}")
                        });
                        assert!(!expected.is_empty(), "empty translation at {path}");
                        assert_ne!(expected, source, "untranslated catalog entry at {path}");
                        assert_eq!(
                            actual.as_str(),
                            Some(expected),
                            "incorrect runtime translation at {path}: {source:?}"
                        );
                        1
                    } else {
                        assert_translated_descriptions(value, actual, catalog, &path)
                    }
                })
                .sum()
        }
        Value::Array(values) => {
            let translated = chinese
                .as_array()
                .unwrap_or_else(|| panic!("missing translated array at {path}"));
            assert_eq!(values.len(), translated.len(), "schema mismatch at {path}");
            values
                .iter()
                .zip(translated)
                .enumerate()
                .map(|(index, (english, chinese))| {
                    assert_translated_descriptions(
                        english,
                        chinese,
                        catalog,
                        &format!("{path}/{index}"),
                    )
                })
                .sum()
        }
        _ => 0,
    }
}

#[test]
fn mcp_全链路端到端() {
    let dir = tempfile::tempdir().expect("临时目录失败");
    #[cfg(unix)]
    let socket = dir.path().join("mcp.sock");
    #[cfg(windows)]
    let socket = std::path::PathBuf::from(format!(
        r"\\.\pipe\flattencom-mcp-test-{}",
        std::process::id()
    ));
    let state_dir = dir.path().join("state");

    // Launch the foreground daemon and clean it up at test exit.
    let mut daemon = Command::new(daemon_path())
        .arg("--foreground")
        .env("FLATTENCOM_SOCKET", &socket)
        .env("FLATTENCOM_STATE_DIR", &state_dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("启动 flattencomd 失败");

    // Wait for daemon readiness.
    let deadline = Instant::now() + Duration::from_secs(10);
    while !state_dir.join("daemon.token").exists() {
        assert!(Instant::now() < deadline, "守护进程未监听");
        std::thread::sleep(Duration::from_millis(50));
    }

    // Launch the MCP subprocess with the same endpoint environment.
    let mut child = Command::new(env!("CARGO_BIN_EXE_flattencom-mcp"))
        .args(["--lang", "en"])
        .env("FLATTENCOM_NO_AUTOSPAWN", "1")
        .env("FLATTENCOM_SOCKET", &socket)
        .env("FLATTENCOM_STATE_DIR", &state_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("启动 flattencom-mcp 失败");
    let stdin = child.stdin.take().expect("无 stdin");
    let stdout = child.stdout.take().expect("无 stdout");
    let mut mcp = McpChild {
        child,
        stdin: Some(stdin),
        stdout: BufReader::new(stdout),
        next_id: 1,
        notifications: Vec::new(),
    };

    // ── initialize ──
    let init = McpChild::must_ok(mcp.roundtrip(
        "initialize",
        serde_json::json!({
            "protocolVersion": PROTOCOL,
            "capabilities": {},
            "clientInfo": { "name": "e2e-test", "version": "0.1.0" },
        }),
    ));
    assert_eq!(init["serverInfo"]["name"], "flattencom-mcp");
    assert!(init["capabilities"]["tools"].is_object(), "应声明工具能力");
    assert!(
        init["capabilities"]["resources"].is_object(),
        "应声明资源能力"
    );
    assert!(
        init["capabilities"]["prompts"].is_object(),
        "应声明提示词能力"
    );
    assert!(
        !init["instructions"].as_str().unwrap_or_default().is_empty(),
        "应有使用说明"
    );
    mcp.notify("notifications/initialized", serde_json::json!({}));

    // Tool catalog
    let tools = McpChild::must_ok(mcp.roundtrip("tools/list", serde_json::json!({})));
    for tool in tools["tools"].as_array().unwrap() {
        if let Some(schema) = tool.get("outputSchema") {
            assert_eq!(
                schema["type"], "object",
                "invalid output schema for {}",
                tool["name"]
            );
        }
    }
    fn assert_english_descriptions(value: &Value) {
        match value {
            Value::Object(map) => {
                for (key, value) in map {
                    if key == "description" && value.is_string() {
                        assert!(
                            !value
                                .as_str()
                                .unwrap()
                                .chars()
                                .any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)),
                            "non-English description: {value}"
                        );
                    } else {
                        assert_english_descriptions(value);
                    }
                }
            }
            Value::Array(values) => {
                for value in values {
                    assert_english_descriptions(value);
                }
            }
            _ => {}
        }
    }
    assert_english_descriptions(&tools);
    // A second MCP process uses Chinese without changing the first client's locale.
    let mut child = Command::new(env!("CARGO_BIN_EXE_flattencom-mcp"))
        .args(["--lang", "zh-CN"])
        .env("FLATTENCOM_SOCKET", &socket)
        .env("FLATTENCOM_STATE_DIR", &state_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let stdin = child.stdin.take();
    let stdout = BufReader::new(child.stdout.take().unwrap());
    let mut chinese = McpChild {
        child,
        stdin,
        stdout,
        next_id: 1,
        notifications: vec![],
    };
    McpChild::must_ok(chinese.roundtrip("initialize",serde_json::json!({"protocolVersion":PROTOCOL,"capabilities":{},"clientInfo":{"name":"language-test","version":"1"}})));
    chinese.notify("notifications/initialized", serde_json::json!({}));
    let translated = McpChild::must_ok(chinese.roundtrip("tools/list", serde_json::json!({})));
    let catalog: Value =
        serde_json::from_str(include_str!("../../flattencom-core/locales/zh-CN.json")).unwrap();
    let english_tools = tools["tools"].as_array().unwrap();
    let chinese_tools = translated["tools"].as_array().unwrap();
    assert_eq!(english_tools.len(), chinese_tools.len());
    let mut description_counts = [0; 3];
    for english in english_tools {
        let name = english["name"].as_str().unwrap();
        let chinese = chinese_tools
            .iter()
            .find(|tool| tool["name"] == name)
            .unwrap_or_else(|| panic!("missing Chinese tool {name}"));
        for (index, key) in ["description", "inputSchema", "outputSchema"]
            .into_iter()
            .enumerate()
        {
            if let Some(value) = english.get(key) {
                // Wrap the root tool description so it follows the same key-based traversal.
                description_counts[index] += assert_translated_descriptions(
                    &serde_json::json!({key: value}),
                    &serde_json::json!({key: chinese[key]}),
                    &catalog,
                    name,
                );
            }
        }
    }
    assert_eq!(description_counts[0], english_tools.len());
    assert!(description_counts[1] > 0, "no input descriptions checked");
    assert!(description_counts[2] > 0, "no output descriptions checked");
    let list = translated["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "list_ports")
        .unwrap();
    assert!(list["description"].as_str().unwrap().contains("串口"));
    let prompt = McpChild::must_ok(chinese.roundtrip(
        "prompts/get",
        serde_json::json!({"name":"analyze-selection","arguments":{"selection_id":"test-id"}}),
    ));
    assert!(prompt.to_string().contains("只读取"));
    chinese.child.kill().unwrap();
    chinese.child.wait().unwrap();
    let names: Vec<&str> = tools["tools"]
        .as_array()
        .expect("tools 数组")
        .iter()
        .map(|t| t["name"].as_str().expect("工具名"))
        .collect();
    assert!(names.len() >= 20, "工具应 ≥ 20 个:{names:?}");
    for must in [
        "list_ports",
        "open_port",
        "send",
        "read_frames",
        "set_decoder",
        "set_signals",
        "set_triggers",
        "get_stats",
        "read_selection",
        "start_transfer",
        "cancel_operation",
        "reset_device",
        "list_markers",
    ] {
        assert!(names.contains(&must), "缺少工具 {must}");
    }
    // Every tool has a description and input schema.
    for t in tools["tools"].as_array().unwrap() {
        assert!(
            !t["description"].as_str().unwrap_or("").is_empty(),
            "工具缺描述:{t}"
        );
        assert!(t["inputSchema"].is_object(), "工具缺 inputSchema:{t}");
    }

    // Resource catalog
    let res = McpChild::must_ok(mcp.roundtrip("resources/list", serde_json::json!({})));
    let uris: Vec<&str> = res["resources"]
        .as_array()
        .expect("resources 数组")
        .iter()
        .map(|r| r["uri"].as_str().expect("uri"))
        .collect();
    for must in [
        "flattencom://ports",
        "flattencom://sessions",
        "flattencom://decoders",
    ] {
        assert!(uris.contains(&must), "缺少资源 {must}");
    }

    // Tool: list ports
    let ports = mcp.call_tool("list_ports", serde_json::json!({}));
    assert!(
        ports["structuredContent"]["ports"].is_array(),
        "结构化端口表"
    );

    // Initial catalog baseline, then open/rename/close invalidations without subscriptions.
    mcp.wait_for_catalog_change();
    mcp.notifications.clear();
    // Tool: open virtual loopback
    let open = mcp.call_tool(
        "open_port",
        serde_json::json!({ "path": "virtual://echo", "label": "mcp-e2e",
            "data_bits":"five", "parity":"mark", "stop_bits":"one_point_five" }),
    );
    let sid = open["structuredContent"]["session"]["session_id"]
        .as_str()
        .expect("session_id")
        .to_owned();
    let stream_uri = format!("flattencom://sessions/{sid}/stream");
    assert_eq!(
        open["structuredContent"]["session"]["config"]["parity"],
        "mark"
    );
    assert_eq!(
        open["structuredContent"]["session"]["config"]["stop_bits"],
        "one_point_five"
    );
    let configured = mcp.call_tool(
        "configure_port",
        serde_json::json!({
            "session_id":sid,"data_bits":"eight","parity":"space","stop_bits":"two"
        }),
    );
    assert_eq!(configured["structuredContent"]["config"]["parity"], "space");
    assert_eq!(
        configured["structuredContent"]["config"]["stop_bits"],
        "two"
    );
    mcp.wait_for_catalog_change();
    let catalog = McpChild::must_ok(mcp.roundtrip("resources/list", serde_json::json!({})));
    assert_eq!(catalog["resources"].as_array().unwrap().len(), 6);
    mcp.notifications.clear();
    mcp.call_tool(
        "configure_port",
        serde_json::json!({"session_id":sid,"label":"renamed"}),
    );
    mcp.wait_for_catalog_change();
    let catalog = McpChild::must_ok(mcp.roundtrip("resources/list", serde_json::json!({})));
    assert!(
        catalog["resources"]
            .as_array()
            .unwrap()
            .iter()
            .any(|resource| { resource["name"].as_str().unwrap().contains("renamed") })
    );
    // Publish exact GUI evidence via RPC, then exercise a selection-only MCP process.
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let rpc = runtime
        .block_on(flattencom_proto::client::DaemonClient::connect_with(
            flattencom_proto::client::ConnectConfig::new("selection-test", Duration::from_secs(3))
                .with_socket(&socket)
                .with_state_dir(&state_dir),
        ))
        .unwrap();
    let published: Value = runtime
        .block_on(rpc.call_typed(
            "publish_selection",
            &serde_json::json!({"session_id":sid,"text":"exact selected evidence"}),
            Duration::from_secs(3),
        ))
        .unwrap();
    let selection_id = published["selection"]["id"].as_str().unwrap();
    assert_eq!(
        mcp.call_tool(
            "read_selection",
            serde_json::json!({"selection_id":selection_id})
        )["structuredContent"]["selection"]["text"],
        "exact selected evidence"
    );
    let mut child = Command::new(env!("CARGO_BIN_EXE_flattencom-mcp"))
        .args(["--selection", selection_id])
        .env("FLATTENCOM_SOCKET", &socket)
        .env("FLATTENCOM_STATE_DIR", &state_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let stdin = child.stdin.take();
    let stdout = BufReader::new(child.stdout.take().unwrap());
    let mut restricted = McpChild {
        child,
        stdin,
        stdout,
        next_id: 1,
        notifications: vec![],
    };
    McpChild::must_ok(restricted.roundtrip("initialize",serde_json::json!({"protocolVersion":PROTOCOL,"capabilities":{},"clientInfo":{"name":"selected","version":"1"}})));
    restricted.notify("notifications/initialized", serde_json::json!({}));
    assert_eq!(
        restricted.call_tool(
            "read_selection",
            serde_json::json!({"selection_id":selection_id})
        )["structuredContent"]["selection"]["text"],
        "exact selected evidence"
    );
    for (name, arguments) in [
        ("read_frames", serde_json::json!({"session_id":sid})),
        (
            "send",
            serde_json::json!({"session_id":sid,"data":"must not send"}),
        ),
        (
            "read_selection",
            serde_json::json!({"selection_id":"other"}),
        ),
    ] {
        let response = McpChild::must_ok(restricted.roundtrip(
            "tools/call",
            serde_json::json!({"name":name,"arguments":arguments}),
        ));
        assert_eq!(response["isError"], true, "scope bypass: {response}");
    }
    let _: Value = runtime
        .block_on(rpc.call_typed(
            "revoke_selection",
            &serde_json::json!({"selection_id":selection_id}),
            Duration::from_secs(3),
        ))
        .unwrap();
    let revoked = McpChild::must_ok(restricted.roundtrip(
        "tools/call",
        serde_json::json!({"name":"read_selection","arguments":{"selection_id":selection_id}}),
    ));
    assert_eq!(revoked["isError"], true);
    restricted.child.kill().unwrap();
    restricted.child.wait().unwrap();
    McpChild::must_ok(mcp.roundtrip(
        "resources/subscribe",
        serde_json::json!({"uri": stream_uri}),
    ));
    assert_eq!(
        open["structuredContent"]["session"]["owner"],
        "flattencom-mcp"
    );

    // Tools: send and read loopback frames
    let send = mcp.call_tool(
        "send",
        serde_json::json!({ "session_id": sid, "data": "PING", "newline": "crlf" }),
    );
    assert_eq!(send["structuredContent"]["bytes_sent"], 6);
    for arguments in [
        serde_json::json!({"session_id":sid}),
        serde_json::json!({"session_id":sid,"format":"decoded"}),
        serde_json::json!({"session_id":sid,"tail":false,"format":"decoded"}),
    ] {
        let sent = mcp.call_tool("read_sent", arguments);
        let frames = sent["structuredContent"]["frames"].as_array().unwrap();
        assert!(frames.iter().any(|frame| frame["text"] == "PING\r\n"));
    }

    let deadline = Instant::now() + Duration::from_secs(5);
    let page = loop {
        let p = mcp.call_tool(
            "read_frames",
            serde_json::json!({ "session_id": sid, "format": "hex" }),
        );
        let frames = p["structuredContent"]["frames"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let has_echo = frames
            .iter()
            .any(|f| f["dir"] == "rx" && f["hex"].as_str().unwrap_or("").contains("50 49 4E 47"));
        if has_echo {
            break p;
        }
        assert!(Instant::now() < deadline, "应收到回环帧:{frames:?}");
        std::thread::sleep(Duration::from_millis(100));
    };
    // Frame pages include cursors and structured fields.
    assert!(
        page["structuredContent"]["next_seq"].is_u64(),
        "next_seq 游标"
    );
    assert!(
        page["structuredContent"]["dropped_rx"].is_u64(),
        "溢出计数可见"
    );

    // Tools: backend decoding
    mcp.call_tool(
        "set_decoder",
        serde_json::json!({ "session_id": sid, "spec": { "name": "ascii_lines", "options": {} } }),
    );
    mcp.call_tool(
        "send",
        serde_json::json!({ "session_id": sid, "data": "AT", "newline": "crlf" }),
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let p = mcp.call_tool("read_frames", serde_json::json!({ "session_id": sid }));
        let frames = p["structuredContent"]["frames"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        if frames
            .iter()
            .any(|f| f["decoded_text"].as_str() == Some("AT␍␊"))
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "应产出可视化解码:{}",
            Value::Array(frames)
        );
        std::thread::sleep(Duration::from_millis(100));
    }

    // Tool: statistics
    let stats = mcp.call_tool("get_stats", serde_json::json!({ "session_id": sid }));
    // Sent bytes: PING+CRLF (6) and AT+CRLF (4), total 10.
    assert!(
        stats["structuredContent"]["stats"]["tx_bytes"]
            .as_u64()
            .unwrap_or(0)
            >= 10
    );

    // Tools: automatic-response triggers
    mcp.call_tool(
        "set_triggers",
        serde_json::json!({ "session_id": sid, "triggers": [{
            "id": "ack",
            "direction": "rx",
            "regex": "PING",
            "actions": [{ "action": "respond", "data": "ACK\r\n" }],
            "min_interval_ms": 0,
        }]}),
    );
    mcp.call_tool(
        "send",
        serde_json::json!({ "session_id": sid, "data": "PING", "newline": "crlf" }),
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let fires = mcp.call_tool(
            "get_trigger_fires",
            serde_json::json!({ "session_id": sid }),
        );
        let fs = fires["structuredContent"]["fires"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        if fs
            .iter()
            .any(|f| f["trigger_id"] == "ack" && f["responded"] == true)
        {
            break;
        }
        assert!(Instant::now() < deadline, "触发器应已应答:{fs:?}");
        std::thread::sleep(Duration::from_millis(100));
    }

    // Read live resources.
    let read = McpChild::must_ok(mcp.roundtrip(
        "resources/read",
        serde_json::json!({ "uri": "flattencom://sessions" }),
    ));
    let text = read["contents"][0]["text"].as_str().expect("资源文本");
    let sessions: Value = serde_json::from_str(text).expect("资源文本应为 JSON");
    assert!(
        sessions["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["session_id"] == sid)
    );

    // Prompts
    let prompts = McpChild::must_ok(mcp.roundtrip("prompts/list", serde_json::json!({})));
    let pnames: Vec<&str> = prompts["prompts"]
        .as_array()
        .expect("prompts 数组")
        .iter()
        .map(|p| p["name"].as_str().expect("提示词名"))
        .collect();
    assert!(pnames.len() >= 4, "提示词应 ≥ 4 个:{pnames:?}");
    for must in [
        "serial-debug-wizard",
        "baud-detect",
        "modbus-analyzer",
        "firmware-flash-esp32",
    ] {
        assert!(pnames.contains(&must), "缺少提示词 {must}");
    }
    let got = McpChild::must_ok(mcp.roundtrip(
        "prompts/get",
        serde_json::json!({
            "name": "serial-debug-wizard",
            "arguments": { "goal": "与设备建立通信" },
        }),
    ));
    assert!(
        !got["messages"].as_array().unwrap().is_empty(),
        "提示词应返回消息"
    );

    // Resource notifications are coalesced; use ordinary tool responses to drive the pipe.
    let deadline = Instant::now() + Duration::from_secs(5);
    while !mcp.notifications.iter().any(|n| {
        n["method"] == "notifications/resources/updated" && n["params"]["uri"] == stream_uri
    }) {
        assert!(
            Instant::now() < deadline,
            "missing resource update notification"
        );
        std::thread::sleep(Duration::from_millis(100));
        mcp.call_tool("get_stats", serde_json::json!({"session_id": sid}));
    }
    McpChild::must_ok(mcp.roundtrip(
        "resources/unsubscribe",
        serde_json::json!({"uri": stream_uri}),
    ));
    // Cleanup: close sessions and exit processes.
    mcp.notifications.clear();
    mcp.call_tool("close_port", serde_json::json!({ "session_id": sid }));
    mcp.wait_for_catalog_change();
    let catalog = McpChild::must_ok(mcp.roundtrip("resources/list", serde_json::json!({})));
    assert_eq!(catalog["resources"].as_array().unwrap().len(), 3);
    McpChild::must_ok(mcp.roundtrip(
        "resources/subscribe",
        serde_json::json!({"uri":"flattencom://sessions"}),
    ));
    let mcp_pid = mcp.child.id();
    daemon.kill().unwrap();
    daemon.wait().unwrap();
    // Failed reconnect must leave stdio alive and allow another attempt later.
    let unavailable = McpChild::must_ok(mcp.roundtrip(
        "tools/call",
        serde_json::json!({"name":"list_sessions","arguments":{}}),
    ));
    assert_eq!(unavailable["isError"], true);
    daemon = Command::new(daemon_path())
        .arg("--foreground")
        .env("FLATTENCOM_SOCKET", &socket)
        .env("FLATTENCOM_STATE_DIR", &state_dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_millis(1200));
    let info = mcp.call_tool("daemon_info", serde_json::json!({}));
    let health = &info["structuredContent"]["backend_connection"];
    assert_eq!(health["connected"], true);
    assert_eq!(health["generation"], 1);
    assert!(health["last_disconnect"]["reason"].is_string());
    assert!(health["last_disconnect"]["at_ms"].as_u64().unwrap() > 0);
    assert_eq!(mcp.child.id(), mcp_pid);
    mcp.notifications.clear();
    let opened = mcp.call_tool("open_port", serde_json::json!({"path":"virtual://echo"}));
    let new_sid = opened["structuredContent"]["session"]["session_id"].clone();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !mcp.notifications.iter().any(|n| {
        n["method"] == "notifications/resources/updated"
            && n["params"]["uri"] == "flattencom://sessions"
    }) {
        assert!(
            Instant::now() < deadline,
            "subscription did not survive backend restart"
        );
        std::thread::sleep(Duration::from_millis(100));
        mcp.call_tool("list_sessions", serde_json::json!({}));
    }
    mcp.call_tool("close_port", serde_json::json!({"session_id":new_sid}));
    drop(mcp.stdin.take()); // Drop stdin to request graceful MCP shutdown.
    let exited = Instant::now() + Duration::from_secs(15);
    loop {
        match mcp.child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < exited => std::thread::sleep(Duration::from_millis(50)),
            _ => panic!("MCP 服务器未退出"),
        }
    }
    daemon.kill().ok();
    let _ = daemon.wait();
}
