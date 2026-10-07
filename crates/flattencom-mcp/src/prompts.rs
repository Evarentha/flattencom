/*
 * flattencom - flattencom Flattencom-Mcp Src Prompts
 *
 * Renders localized debugging prompt templates without interpreting substituted user values.
 *
 * Authors:
 * worryzu <worryzu@gmail.com> @LinearTeam
 *
 * Copyright (C) 2026 Evarentha
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

//! Localized MCP debugging prompts. English is the source language.
use crate::server::FlattencomMcp;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{PromptMessage, Role};
use rmcp::{prompt, prompt_router};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DebugWizardParams {
    /// Debugging goal.
    pub goal: String,
    /// Known device configuration and observations.
    pub hints: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct BaudDetectParams {
    /// Serial port path.
    pub port: String,
    /// Capture duration per candidate rate, in milliseconds.
    pub duration_ms: Option<u64>,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ModbusAnalyzerParams {
    /// Capture role: auto, master or slave.
    pub role: Option<String>,
    /// Device address in hexadecimal.
    pub slave_address: Option<String>,
    /// First register in decimal.
    pub start_register: Option<u32>,
    /// Number of registers.
    pub register_count: Option<u32>,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct FlashEsp32Params {
    /// Serial port path.
    pub port: String,
    /// Firmware file path.
    pub firmware_path: String,
    /// Flashing baud rate (default 921600).
    pub baud_rate: Option<u32>,
}
fn render(name: &str, values: &[(&str, String)]) -> Vec<PromptMessage> {
    let catalog: serde_json::Value =
        serde_json::from_str(include_str!("../locales/prompts.en.json"))
            .expect("valid prompt catalog");
    let source = catalog[name]["body"].as_str().expect("prompt body");
    let template = flattencom_core::i18n::text(source);
    // Substitute tokens in the template only. Values are never parsed as templates.
    let mut output = String::new();
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        output.push_str(&rest[..start]);
        rest = &rest[start + 1..];
        if let Some(end) = rest.find('}') {
            let key = &rest[..end];
            if let Some((_, value)) = values.iter().find(|(name, _)| *name == key) {
                output.push_str(value);
            } else {
                output.push('{');
                output.push_str(key);
                output.push('}');
            }
            rest = &rest[end + 1..];
        } else {
            output.push('{');
            break;
        }
    }
    output.push_str(rest);
    vec![PromptMessage::new_text(Role::User, output)]
}
#[prompt_router(router = "prompt_router", vis = "pub")]
impl FlattencomMcp {
    #[prompt(
        name = "analyze-selection",
        description = "Analyze only the selected log text, citing observations and distinguishing hypotheses."
    )]
    pub async fn analyze_selection(
        &self,
        p: Parameters<crate::tools::SelectionParams>,
    ) -> Vec<PromptMessage> {
        render("analyze-selection", &[("selection_id", p.0.selection_id)])
    }
    #[prompt(
        name = "serial-debug-wizard",
        description = "Diagnose a serial connection using device information, configuration and captured data."
    )]
    pub async fn serial_debug_wizard(
        &self,
        p: Parameters<DebugWizardParams>,
    ) -> Vec<PromptMessage> {
        render(
            "serial-debug-wizard",
            &[
                ("goal", p.0.goal),
                ("hints", p.0.hints.unwrap_or_else(|| "None".into())),
            ],
        )
    }
    #[prompt(
        name = "baud-detect",
        description = "Estimate baud rate by comparing received text at candidate rates."
    )]
    pub async fn baud_detect(&self, p: Parameters<BaudDetectParams>) -> Vec<PromptMessage> {
        render(
            "baud-detect",
            &[
                ("port", p.0.port),
                ("duration_ms", p.0.duration_ms.unwrap_or(800).to_string()),
            ],
        )
    }
    #[prompt(
        name = "modbus-analyzer",
        description = "Inspect Modbus RTU CRCs, register data and exception codes."
    )]
    pub async fn modbus_analyzer(&self, p: Parameters<ModbusAnalyzerParams>) -> Vec<PromptMessage> {
        render(
            "modbus-analyzer",
            &[
                ("role", p.0.role.unwrap_or_else(|| "auto".into())),
                (
                    "slave_address",
                    p.0.slave_address.unwrap_or_else(|| "01".into()),
                ),
                (
                    "start_register",
                    p.0.start_register.unwrap_or(0).to_string(),
                ),
                (
                    "register_count",
                    p.0.register_count.unwrap_or(1).to_string(),
                ),
            ],
        )
    }
    #[prompt(
        name = "firmware-flash-esp32",
        description = "Flash an ESP32 with esptool and inspect its boot log."
    )]
    pub async fn firmware_flash_esp32(
        &self,
        p: Parameters<FlashEsp32Params>,
    ) -> Vec<PromptMessage> {
        render(
            "firmware-flash-esp32",
            &[
                ("port", p.0.port),
                ("firmware_path", p.0.firmware_path),
                ("baud_rate", p.0.baud_rate.unwrap_or(921_600).to_string()),
            ],
        )
    }
}
