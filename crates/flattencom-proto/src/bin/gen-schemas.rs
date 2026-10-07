/*
 * flattencom - flattencom Flattencom-Proto Src Bin Gen-Schemas
 *
 * Generates public JSON Schemas from the Rust RPC request, response and session types.
 *
 * Authors:
 * worryzu <worryzu@gmail.com> @LinearTeam
 *
 * Copyright (C) 2026 Evarentha
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

//! Generate public JSON Schemas for protocol consumers.

use std::fs;
use std::path::PathBuf;

use schemars::schema_for;

use flattencom_proto::methods::{
    Event, FrameOut, HelloParams, ListPortsResult, OpenSessionParams, ReadFramesParams,
    RpcNotification, RpcRequest, RpcResponse, SendParams, SessionSummary,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../schemas");
    fs::create_dir_all(&root)?;
    let schemas = [
        ("rpc-request", schema_for!(RpcRequest)),
        ("rpc-response", schema_for!(RpcResponse)),
        ("rpc-notification", schema_for!(RpcNotification)),
        ("event", schema_for!(Event)),
        ("hello", schema_for!(HelloParams)),
        ("list-ports-result", schema_for!(ListPortsResult)),
        ("open-session", schema_for!(OpenSessionParams)),
        ("send", schema_for!(SendParams)),
        ("read-frames", schema_for!(ReadFramesParams)),
        ("frame", schema_for!(FrameOut)),
        ("session", schema_for!(SessionSummary)),
        (
            "transfer",
            schema_for!(flattencom_proto::workbench::TransferParams),
        ),
    ];
    for (name, schema) in schemas {
        fs::write(
            root.join(format!("{name}.json")),
            serde_json::to_vec_pretty(&schema)?,
        )?;
    }
    println!("generated {} schemas in {}", 12, root.display());
    Ok(())
}
