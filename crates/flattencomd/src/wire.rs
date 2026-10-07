/*
 * flattencom - Bounded Daemon Wire Output
 *
 * Serializes bounded replies and best-effort notifications for the local service.
 *
 * Authors:
 * worryzu <worryzu@gmail.com> @LinearTeam
 *
 * Copyright (C) 2026 Evarentha
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

//! Outbound framing budgets include the newline added by the connection writer.

use std::io::{self, Write};

use flattencom_proto::client::MAX_LINE_BYTES;
use flattencom_proto::methods::{RpcError, RpcNotification, RpcResponse, error_code};
use serde::Serialize;
use serde_json::Value;

struct BoundedOutput(Vec<u8>);

impl Write for BoundedOutput {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() >= MAX_LINE_BYTES - self.0.len() {
            return Err(io::Error::other("RPC output exceeds message limit"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn encode(value: &impl Serialize) -> Result<String, serde_json::Error> {
    let mut output = BoundedOutput(Vec::new());
    serde_json::to_writer(&mut output, value)?;
    Ok(String::from_utf8(output.0).expect("JSON serialization produces UTF-8"))
}

/// Preserve correlation even when a result or an error exceeds the wire budget.
/// Never truncate a successful result or include the oversized payload in the error.
pub fn response(id: u64, result: Result<Value, RpcError>) -> String {
    let (result, error) = match result {
        Ok(value) => (value, None),
        Err(error) => (Value::Null, Some(error)),
    };
    encode(&RpcResponse {
        jsonrpc: "2.0".into(),
        id,
        result,
        error,
    })
    .unwrap_or_else(|_| {
        encode(&RpcResponse {
            jsonrpc: "2.0".into(),
            id,
            result: Value::Null,
            error: Some(RpcError {
                code: error_code::INTERNAL,
                message: "RPC response exceeds the message limit or could not be serialized".into(),
                data: Value::Null,
            }),
        })
        .expect("fixed error response fits the wire budget")
    })
}

/// Notifications are best effort; oversized events are dropped without closing the connection.
pub fn notification(note: &RpcNotification) -> Option<String> {
    match encode(note) {
        Ok(line) => Some(line),
        Err(error) => {
            tracing::warn!(method = %note.method, %error, "Dropping unencodable RPC notification");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn aggregate_results_and_errors_remain_correlated_and_bounded() {
        // Individually valid labels can still exceed the aggregate response budget.
        let sessions = vec![json!({"label":"x".repeat(4096)}); 4096];
        for result in [
            Ok(json!({"sessions":sessions})),
            Err(RpcError {
                code: -32601,
                message: "x".repeat(MAX_LINE_BYTES),
                data: Value::Null,
            }),
        ] {
            let line = response(u64::MAX, result);
            assert!(line.len() < MAX_LINE_BYTES);
            let reply: Value = serde_json::from_str(&line).unwrap();
            assert_eq!(reply["id"], u64::MAX);
            assert_eq!(reply["error"]["code"], error_code::INTERNAL);
            assert!(reply.get("result").is_none());
        }
    }

    #[test]
    fn exact_newline_budget_and_oversized_notifications() {
        let text = "x".repeat(MAX_LINE_BYTES - 3); // Quotes plus newline.
        assert_eq!(encode(&text).unwrap().len() + 1, MAX_LINE_BYTES);
        assert!(encode(&(text + "x")).is_err());
        assert!(
            notification(&RpcNotification {
                jsonrpc: "2.0".into(),
                method: "frames".into(),
                params: json!({"text":"x".repeat(MAX_LINE_BYTES)}),
            })
            .is_none()
        );
    }
}
