/*
 * flattencom - flattencom Flattencom-Mcp Src Main
 *
 * Starts the localized stdio MCP server and connects it to the serial background service.
 *
 * Authors:
 * worryzu <worryzu@gmail.com> @LinearTeam
 *
 * Copyright (C) 2026 Evarentha
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

//! flattencom MCP server entry point.
//!
//! Runs as a standard MCP stdio subprocess launched by Claude Code, Claude Desktop,
//! or another MCP client; connect_or_spawn starts the daemon when needed.
//!
//! stdout is reserved for MCP; all diagnostics go to stderr.

#![forbid(unsafe_code)]
#![allow(clippy::too_many_lines)]

mod backend;
mod flash;
mod prompts;
mod server;
mod tools;

use std::time::Duration;

use rmcp::ServiceExt;
use tracing_subscriber::EnvFilter;

use flattencom_proto::client::{ConnectConfig, connect_or_spawn_with};

fn main() -> anyhow::Result<()> {
    flattencom_core::i18n::init();
    // Log to stderr because MCP owns stdout.
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .with_target(false)
        .init();

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;

    runtime.block_on(main_async())
}

async fn main_async() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let mut selection = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--selection" => {
                selection = Some(
                    args.next()
                        .ok_or_else(|| anyhow::anyhow!("--selection requires an ID"))?,
                );
            }
            "--lang" => {
                let value = args
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--lang requires en or zh-CN"))?;
                let language = flattencom_core::i18n::Language::parse(&value)
                    .ok_or_else(|| anyhow::anyhow!("Invalid language: {value}"))?;
                flattencom_core::i18n::set_default(language);
            }
            "--help" | "-h" => {
                eprintln!(
                    "flattencom-mcp [--lang en|zh-CN] [--selection ID]\n--selection restricts this process to one GUI-published snapshot."
                );
                return Ok(());
            }
            arg => anyhow::bail!("unknown argument: {arg}"),
        }
    }
    // Connect or start the daemon; the client label identifies session ownership.
    let cfg = ConnectConfig::new("flattencom-mcp", Duration::from_secs(3));
    let daemon = connect_or_spawn_with(cfg.clone()).await.map_err(|e| {
        anyhow::anyhow!("Failed to connect to flattencomd: {e}. Try starting flattencomd manually.")
    })?;
    tracing::info!(socket = %flattencom_proto::socket::socket_path().display(), "Connected to background service");

    // MCP service over stdio
    let service = server::FlattencomMcp::new(backend::Backend::new(daemon, cfg), selection)
        .serve(rmcp::transport::stdio())
        .await
        .map_err(|e| anyhow::anyhow!("Failed to start MCP service: {e}"))?;
    tracing::info!("flattencom-mcp ready (stdio)");
    service
        .waiting()
        .await
        .map_err(|e| anyhow::anyhow!("MCP service exited with an error: {e}"))?;
    Ok(())
}
