/*
 * flattencom - CLI Main
 *
 * Declares CLI arguments, selects language and initializes command dispatch and logging.
 *
 * Authors:
 * worryzu <worryzu@gmail.com> @LinearTeam
 *
 * Copyright (C) 2026 Evarentha
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

//! flattencom CLI entry point: subcommand declarations and dispatch.
//!
//! Commands fall into two groups:
//! - **Direct** (`list/send/receive/monitor/autobaud/replay/decode`):
//!   embed the core engine without a daemon, supporting scripts and pipelines with fast startup;
//! - **Shared** (`attach/sessions/daemon`): use the daemon alongside GUI and MCP clients.

#![forbid(unsafe_code)]
#![allow(clippy::too_many_lines)]

mod cmd;
mod tui;

use std::ffi::OsString;
use std::path::PathBuf;

use clap::{CommandFactory, FromArgMatches, Parser, Subcommand};
use clap_complete::Shell;

/// flattencom serial debugging tool.
#[derive(Parser)]
#[command(
    name = "flattencom",
    version,
    about = "Serial debugging workbench for Linux and Windows: a Qt desktop, CLI/TUI and MCP bridge over a shared local background service",
    arg_required_else_help = true,
    disable_help_subcommand = true
)]
struct Cli {
    /// Display language: en or zh-CN. Defaults to FLATTENCOM_LANG or en.
    #[arg(long, global = true, value_parser = ["en", "zh-CN", "zh"])]
    lang: Option<String>,
    /// Output JSON.
    #[arg(long, global = true)]
    json: bool,
    /// Write verbose logs to stderr.
    #[arg(short = 'v', long, global = true)]
    verbose: bool,
    /// Background service socket path.
    #[arg(long, global = true, value_name = "PATH")]
    socket: Option<PathBuf>,
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// List serial ports and device information.
    List,
    /// Watch device connections; press Ctrl-C to exit.
    Watch {
        /// Polling interval in milliseconds.
        #[arg(long, default_value_t = 1500)]
        interval_ms: u64,
    },
    /// Interactive terminal monitor (direct device connection).
    Monitor {
        /// Port path; omit to list available ports.
        port: Option<String>,
        /// Baud rate.
        #[arg(short = 'b', long, default_value_t = 115_200)]
        baud: u32,
        /// Data bits: 5, 6, 7 or 8.
        #[arg(short = 'd', long, value_parser = parse_data_bits)]
        data_bits: Option<u8>,
        /// Parity: none, odd, even, mark or space.
        #[arg(short = 'p', long)]
        parity: Option<String>,
        /// Stop bits: 1, 1.5 or 2 (1.5 requires 5 data bits).
        #[arg(short = 't', long, value_parser = parse_stop_bits)]
        stop_bits: Option<flattencom_core::config::StopBits>,
        /// Flow control: none, software or hardware.
        #[arg(short = 'f', long)]
        flow: Option<String>,
        /// Record traffic and markers to a .log file.
        #[arg(long)]
        record_to: Option<PathBuf>,
    },
    /// Send data and optionally wait for a response (direct connection).
    Send {
        /// Port path.
        port: String,
        /// Text payload; mutually exclusive with --hex and --file.
        data: Option<String>,
        /// Hexadecimal payload; spaces are allowed.
        #[arg(long)]
        hex: Option<String>,
        /// Send the contents of a file.
        #[arg(long)]
        file: Option<PathBuf>,
        /// Baud rate.
        #[arg(short = 'b', long, default_value_t = 115_200)]
        baud: u32,
        /// Line ending: auto, none, lf, crlf or cr. Auto appends CRLF to text only.
        #[arg(long, default_value = "auto")]
        newline: String,
        /// Wait for a response matching this regular expression.
        #[arg(long)]
        expect: Option<String>,
        /// Response timeout in milliseconds.
        #[arg(long, default_value_t = 3000)]
        timeout_ms: u64,
    },
    /// Receive data and write it to stdout.
    Receive {
        /// Port path.
        port: String,
        /// Baud rate.
        #[arg(short = 'b', long, default_value_t = 115_200)]
        baud: u32,
        /// Output hexadecimal bytes.
        #[arg(long)]
        hex: bool,
        /// Append output to a file.
        #[arg(short = 'o', long)]
        output: Option<PathBuf>,
        /// Receive duration in seconds; omit to run until Ctrl-C.
        #[arg(long)]
        duration: Option<u64>,
    },
    /// Attach to a shared session.
    Attach {
        /// Session ID; selected automatically if only one exists.
        session: Option<String>,
    },
    /// List shared sessions.
    Sessions,
    /// Call a background service RPC method.
    Rpc {
        /// Method name; see docs/PROTOCOL.md.
        method: String,
        /// JSON parameter object.
        #[arg(default_value = "{}")]
        params: String,
    },
    /// Estimate baud rate from received text.
    Autobaud {
        /// Port path.
        port: String,
        /// Capture duration per baud rate in milliseconds.
        #[arg(long, default_value_t = 800)]
        duration_ms: u64,
        /// Comma-separated candidate baud rates; defaults to common rates.
        #[arg(long)]
        bauds: Option<String>,
    },
    /// Decode a capture file.
    Decode {
        /// JSONL capture file.
        #[arg(required_unless_present = "list")]
        log: Option<PathBuf>,
        /// Decoder name; see flattencom decode --list.
        #[arg(short = 'd', long = "decoder")]
        decoder: Option<String>,
        /// List available decoders.
        #[arg(long)]
        list: bool,
    },
    /// Replay captured data to a serial port at the given speed.
    Replay {
        /// JSONL capture file.
        log: PathBuf,
        /// Target port path.
        port: String,
        /// Baud rate.
        #[arg(short = 'b', long, default_value_t = 115_200)]
        baud: u32,
        /// Playback speed multiplier.
        #[arg(long, default_value_t = 1.0)]
        speed: f64,
    },
    /// Manage the background service.
    Daemon {
        #[command(subcommand)]
        action: DaemonAction,
    },
    /// Generate shell completions.
    Completion {
        /// Target shell.
        shell: Shell,
    },
}

#[derive(Subcommand)]
enum DaemonAction {
    /// Start the background service or show its status if running.
    Start,
    /// Stop the background service and close all sessions.
    Stop,
    /// Show background service status.
    Status,
    /// Show recent background service logs.
    Logs {
        /// Number of lines.
        #[arg(short = 'n', long, default_value_t = 40)]
        lines: usize,
    },
}

fn parse_data_bits(s: &str) -> Result<u8, String> {
    match s {
        "5" | "6" | "7" | "8" => Ok(s.parse().unwrap()),
        other => Err(flattencom_core::tr!(
            "Invalid data bits {other:?}; choose 5, 6, 7 or 8",
            other = other
        )),
    }
}

fn parse_stop_bits(value: &str) -> Result<flattencom_core::config::StopBits, String> {
    use flattencom_core::config::StopBits;
    match value {
        "1" => Ok(StopBits::One),
        "1.5" => Ok(StopBits::OnePointFive),
        "2" => Ok(StopBits::Two),
        _ => Err(flattencom_core::i18n::text("Stop bits must be 1, 1.5 or 2").into()),
    }
}

fn main() {
    flattencom_core::i18n::init();
    let args: Vec<OsString> = std::env::args_os().collect();
    for (i, arg) in args.iter().enumerate() {
        if arg == "--" {
            break;
        }
        let value = if arg == "--lang" {
            args.get(i + 1).and_then(|value| value.to_str())
        } else {
            arg.to_str().and_then(|value| value.strip_prefix("--lang="))
        };
        if let Some(language) = value.and_then(flattencom_core::i18n::Language::parse) {
            flattencom_core::i18n::set_default(language);
        }
    }
    let cli = match parse_cli(args) {
        Ok(cli) => cli,
        Err(error) => {
            let code = parse_exit_code(&error);
            let _ = error.print();
            std::process::exit(code);
        }
    };
    init_logging(cli.verbose);
    let out = std::io::stdout();
    let code = cmd::dispatch(cli, out);
    std::process::exit(code);
}

fn parse_cli(args: Vec<OsString>) -> Result<Cli, clap::Error> {
    let mut command = Cli::command();
    command.build();
    let matches = localize_help(command).try_get_matches_from(args)?;
    Cli::from_arg_matches(&matches)
}

fn parse_exit_code(error: &clap::Error) -> i32 {
    if error.use_stderr() {
        exit_code::ARGS
    } else {
        exit_code::OK
    }
}

fn localize_help(mut command: clap::Command) -> clap::Command {
    fn help_text(value: &str) -> String {
        let translated = flattencom_core::i18n::text(value);
        if translated != value {
            return translated.to_owned();
        }
        let punctuated = format!("{value}.");
        let translated = flattencom_core::i18n::text(&punctuated);
        if translated == punctuated {
            value.to_owned()
        } else {
            translated.to_owned()
        }
    }
    if let Some(about) = command.get_about() {
        let value = about.to_string();
        command = command.about(help_text(&value));
    }
    command = command.mut_args(|arg| {
        let help = arg.get_help().map(ToString::to_string);
        let long = arg.get_long_help().map(ToString::to_string);
        let mut arg = arg;
        if let Some(value) = help {
            arg = arg.help(help_text(&value));
        }
        if let Some(value) = long {
            arg = arg.long_help(help_text(&value));
        }
        if flattencom_core::i18n::language() == flattencom_core::i18n::Language::Chinese {
            arg = arg
                .help_heading(flattencom_core::i18n::text("Options"))
                .hide_possible_values(true);
        }
        arg
    });
    if flattencom_core::i18n::language() == flattencom_core::i18n::Language::Chinese {
        command = command
            .help_template(
                flattencom_core::i18n::text(
                    "{about-with-newline}\nUsage: {usage}\n\n{all-args}{after-help}",
                )
                .to_owned(),
            )
            .subcommand_help_heading(flattencom_core::i18n::text("Commands"));
    }
    command.mut_subcommands(localize_help)
}

fn init_logging(verbose: bool) {
    let level = if verbose { "debug" } else { "warn" };
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(level)),
        )
        .with_writer(std::io::stderr)
        .with_target(false)
        .init();
}

/// Exit codes (see DEVELOPMENT.md): 0 success, 1 general failure, 2 port busy,
/// 3 permission denied, 4 timeout/no response, 5 invalid arguments.
pub mod exit_code {
    /// Success.
    pub const OK: i32 = 0;
    /// General failure.
    pub const FAIL: i32 = 1;
    /// Port busy.
    pub const BUSY: i32 = 2;
    /// Permission denied.
    pub const PERMISSION: i32 = 3;
    /// Timeout or no response.
    pub const TIMEOUT: i32 = 4;
    /// Invalid arguments.
    pub const ARGS: i32 = 5;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
        parse_cli(args.iter().map(OsString::from).collect())
    }

    #[test]
    fn monitor_rejects_invalid_stop_bits_before_dispatch() {
        for value in ["0", "3", "255"] {
            let error = parse(&["flattencom", "monitor", "unused-port", "--stop-bits", value])
                .err()
                .expect("invalid stop bits must fail before opening the port");
            assert_eq!(error.kind(), clap::error::ErrorKind::ValueValidation);
            assert_eq!(parse_exit_code(&error), exit_code::ARGS);
            assert!(error.to_string().contains("--stop-bits"));
        }
        use flattencom_core::config::StopBits;
        for (value, expected) in [
            ("1", StopBits::One),
            ("1.5", StopBits::OnePointFive),
            ("2", StopBits::Two),
        ] {
            let cli =
                parse(&["flattencom", "monitor", "unused-port", "--stop-bits", value]).unwrap();
            assert!(
                matches!(cli.command, Commands::Monitor { stop_bits: Some(bits), .. } if bits == expected)
            );
        }
    }

    #[test]
    fn parser_exit_codes_distinguish_arguments_from_help() {
        for args in [
            vec!["flattencom", "--unknown-option"],
            vec!["flattencom", "send"],
            vec!["flattencom", "receive", "unused-port", "--baud", "invalid"],
            vec!["flattencom"],
        ] {
            let error = parse(&args).err().expect("expected argument error");
            assert!(error.use_stderr());
            assert_eq!(parse_exit_code(&error), exit_code::ARGS);
        }
        for args in [
            vec!["flattencom", "--help"],
            vec!["flattencom", "monitor", "--help"],
            vec!["flattencom", "--version"],
        ] {
            let error = parse(&args).err().expect("expected help or version output");
            assert!(!error.use_stderr());
            assert_eq!(parse_exit_code(&error), exit_code::OK);
        }
    }
}
