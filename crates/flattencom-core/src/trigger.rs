/*
 * flattencom - Core Trigger
 *
 * Evaluates rate-limited frame matches and prepares responses or external program actions.
 *
 * Authors:
 * worryzu <worryzu@gmail.com> @LinearTeam
 *
 * Copyright (C) 2026 Evarentha
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

//! Triggers perform actions on matching frames, including responses and event recording.
//!
//! Automated responses handle boot banners, handshakes and error-reset exchanges
//! promptly, leaving higher-level decisions to users and clients.
//!
//! min_interval_ms (default 100 ms) limits repeated actions during traffic bursts.

use std::time::{Duration, Instant};

use regex::Regex;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::FlattenError;
use crate::frame::{Direction, Frame};

const MAX_ID_BYTES: usize = 256;
const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
const MAX_ACTIONS: usize = 32;
const MAX_RULE_BYTES: usize = 4 * 1024 * 1024;

/// Trigger action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum TriggerAction {
    /// Send a response using either text or hexadecimal bytes.
    Respond {
        /// Text payload.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        data: Option<String>,
        /// Hexadecimal payload, mutually exclusive with data.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        hex: Option<String>,
    },
    /// Record a trigger_fired event.
    Highlight,
    /// Run a program with fixed arguments, without a shell.
    Execute {
        /// Program path.
        program: String,
        /// Argument list.
        #[serde(default)]
        args: Vec<String>,
    },
}

/// Trigger specification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TriggerSpec {
    /// Client-defined trigger ID included in trigger_fired events.
    pub id: String,
    /// Direction to match (default RX).
    #[serde(default = "default_rx")]
    pub direction: Direction,
    /// Regular expression matched against visible frame text.
    pub regex: String,
    /// Actions to perform.
    pub actions: Vec<TriggerAction>,
    /// Minimum interval between trigger actions in milliseconds.
    #[serde(default = "default_min_interval_ms")]
    pub min_interval_ms: u64,
}

fn default_rx() -> Direction {
    Direction::Rx
}
fn default_min_interval_ms() -> u64 {
    100
}

/// Compiled trigger.
#[derive(Debug)]
pub struct Trigger {
    /// Original specification.
    pub spec: TriggerSpec,
    regex: Regex,
    last_fire: Option<Instant>,
}

impl Trigger {
    /// Compile and return structured regex errors.
    pub fn compile(spec: TriggerSpec) -> Result<Self, FlattenError> {
        if spec.id.is_empty() {
            return Err(FlattenError::InvalidConfig {
                field: "trigger.id".into(),
                reason: "must not be empty".into(),
            });
        }
        if spec.id.len() > MAX_ID_BYTES {
            return Err(FlattenError::InvalidConfig {
                field: "trigger.id".into(),
                reason: "trigger ID exceeds 256 UTF-8 bytes".into(),
            });
        }
        if spec.actions.len() > MAX_ACTIONS {
            return Err(FlattenError::InvalidConfig {
                field: "trigger.actions".into(),
                reason: "trigger exceeds 32 actions".into(),
            });
        }
        let mut rule_bytes = spec.id.len().saturating_add(spec.regex.len());
        for action in &spec.actions {
            match action {
                TriggerAction::Respond { data, hex } => {
                    rule_bytes = rule_bytes
                        .saturating_add(data.as_ref().map_or(0, String::len))
                        .saturating_add(hex.as_ref().map_or(0, String::len));
                }
                TriggerAction::Execute { program, args } => {
                    rule_bytes = rule_bytes.saturating_add(program.len());
                    for arg in args {
                        rule_bytes = rule_bytes.saturating_add(arg.len());
                    }
                }
                TriggerAction::Highlight => {}
            }
        }
        if rule_bytes > MAX_RULE_BYTES {
            return Err(FlattenError::InvalidConfig {
                field: "trigger".into(),
                reason: "trigger strings exceed 4 MiB in total".into(),
            });
        }
        for action in &spec.actions {
            if let TriggerAction::Respond { data, hex } = action {
                if data.is_some() == hex.is_some() {
                    return Err(FlattenError::InvalidConfig {
                        field: "trigger.respond".into(),
                        reason: "exactly one of data or hex is required".into(),
                    });
                }
                let oversized = data
                    .as_ref()
                    .is_some_and(|text| text.len() > MAX_RESPONSE_BYTES)
                    || hex.as_ref().is_some_and(|text| {
                        text.bytes().filter(|b| !b.is_ascii_whitespace()).count()
                            > MAX_RESPONSE_BYTES * 2
                    });
                if oversized {
                    return Err(FlattenError::InvalidConfig {
                        field: "trigger.respond".into(),
                        reason: "trigger response exceeds 1 MiB".into(),
                    });
                }
                if let Some(h) = hex {
                    hex::decode(
                        h.chars()
                            .filter(|c| !c.is_ascii_whitespace())
                            .collect::<String>(),
                    )
                    .map_err(|e| FlattenError::InvalidConfig {
                        field: "trigger.hex".into(),
                        reason: e.to_string(),
                    })?;
                }
            }
        }
        let regex = Regex::new(&spec.regex).map_err(|e| FlattenError::InvalidConfig {
            field: "trigger.regex".into(),
            reason: e.to_string(),
        })?;
        Ok(Self {
            spec,
            regex,
            last_fire: None,
        })
    }

    /// Evaluate a frame unless its direction differs or the rate limit suppresses it.
    ///
    /// Return response bytes for the caller to send; evaluation performs no writes itself.
    #[must_use]
    fn try_fire(&mut self, frame: &Frame) -> Option<TriggerMatch> {
        if frame.dir != self.spec.direction {
            return None;
        }
        if let Some(last) = self.last_fire
            && last.elapsed() < Duration::from_millis(self.spec.min_interval_ms)
        {
            return None;
        }
        let text = frame.text_visual();
        let m = self.regex.find(&text).or_else(|| {
            frame
                .decoded
                .as_ref()
                .and_then(|d| self.regex.find(&d.text))
        })?;
        self.last_fire = Some(Instant::now());
        Some(TriggerMatch {
            trigger_id: self.spec.id.clone(),
            matched: m.as_str().chars().take(96).collect(),
            responses: self.collect_responses(),
            commands: self
                .spec
                .actions
                .iter()
                .filter_map(|a| match a {
                    TriggerAction::Execute { program, args } => {
                        Some((program.clone(), args.clone()))
                    }
                    _ => None,
                })
                .collect(),
        })
    }

    #[must_use]
    fn collect_responses(&self) -> Vec<Vec<u8>> {
        self.spec
            .actions
            .iter()
            .filter_map(|a| match a {
                TriggerAction::Respond { data, hex } => {
                    Some(build_bytes(data.as_deref(), hex.as_deref()))
                }
                TriggerAction::Highlight | TriggerAction::Execute { .. } => None,
            })
            .collect()
    }
}

fn build_bytes(data: Option<&str>, hexs: Option<&str>) -> Vec<u8> {
    if let Some(h) = hexs
        && let Ok(b) = hex::decode(
            h.chars()
                .filter(|c| !c.is_ascii_whitespace())
                .collect::<String>(),
        )
    {
        return b;
    }
    data.map(str::as_bytes)
        .map(<[u8]>::to_vec)
        .unwrap_or_default()
}

/// Trigger match result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TriggerMatch {
    /// Matched trigger ID.
    pub trigger_id: String,
    /// Bounded matched-text excerpt.
    pub matched: String,
    /// Queued response payloads.
    #[serde(skip)]
    pub responses: Vec<Vec<u8>>,
    /// Programs and arguments requested by a match.
    #[serde(skip)]
    pub commands: Vec<(String, Vec<String>)>,
}

/// Trigger history; the session retains the latest 100 entries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TriggerFire {
    /// Matched trigger ID.
    pub trigger_id: String,
    /// Matched frame sequence.
    pub seq: u64,
    /// Match time in UNIX microseconds.
    pub t_us: i64,
    /// Matched text.
    pub matched: String,
    /// Whether a response was sent; false if the response queue was full.
    pub responded: bool,
}

/// Evaluate each trigger against the frame.
///
/// Responses are not sent here because evaluation holds session state;
/// the caller submits them through the nonblocking command queue.
#[must_use]
pub fn evaluate(triggers: &mut [Trigger], frame: &Frame) -> Vec<TriggerMatch> {
    triggers
        .iter_mut()
        .filter_map(|t| t.try_fire(frame))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(dir: Direction, text: &str) -> Frame {
        Frame::new(1, dir, text.as_bytes().to_vec(), 0, 0)
    }

    fn spec(actions: Vec<TriggerAction>) -> TriggerSpec {
        TriggerSpec {
            id: "limit-test".into(),
            direction: Direction::Rx,
            regex: "(?s).*".into(),
            actions,
            min_interval_ms: 0,
        }
    }

    #[test]
    fn trigger_id_limit_counts_utf8_bytes_and_bounds_fire_history() {
        let mut rule = spec(vec![TriggerAction::Highlight]);
        rule.id = "🦀".repeat(64);
        let mut trigger = Trigger::compile(rule.clone()).unwrap();
        let hit = trigger
            .try_fire(&frame(Direction::Rx, &"🦀".repeat(100)))
            .unwrap();
        assert_eq!(hit.trigger_id, rule.id);
        assert_eq!(hit.matched, "🦀".repeat(96));
        rule.id.push('x');
        assert!(
            matches!(Trigger::compile(rule), Err(FlattenError::InvalidConfig { field, .. }) if field == "trigger.id")
        );

        // JSON control-byte escaping is larger than unescaped multibyte UTF-8.
        let fire = TriggerFire {
            trigger_id: "\0".repeat(MAX_ID_BYTES),
            seq: u64::MAX,
            t_us: i64::MIN,
            matched: "\0".repeat(96),
            responded: false,
        };
        let history = serde_json::json!({"fires": vec![fire; 100]});
        assert!(serde_json::to_vec(&history).unwrap().len() < 256 * 1024);
    }

    #[test]
    fn trigger_response_limits_preserve_exact_text_and_hex_payloads() {
        let text = "🦀".repeat(MAX_RESPONSE_BYTES / 4);
        let hex = "41 \t".repeat(MAX_RESPONSE_BYTES / 2) + &"41".repeat(MAX_RESPONSE_BYTES / 2);
        for (data, hex, expected) in [
            (Some(text.clone()), None, text.into_bytes()),
            (None, Some(hex), vec![b'A'; MAX_RESPONSE_BYTES]),
        ] {
            let rule = spec(vec![TriggerAction::Respond { data, hex }]);
            let mut trigger = Trigger::compile(rule.clone()).unwrap();
            assert_eq!(
                trigger
                    .try_fire(&frame(Direction::Rx, "go"))
                    .unwrap()
                    .responses,
                vec![expected]
            );
            let mut oversized = rule;
            if let TriggerAction::Respond { data, hex } = &mut oversized.actions[0] {
                if let Some(data) = data {
                    data.push('x');
                }
                if let Some(hex) = hex {
                    hex.push_str("41");
                }
            }
            assert!(
                matches!(Trigger::compile(oversized), Err(FlattenError::InvalidConfig { field, .. }) if field == "trigger.respond")
            );
        }
    }

    #[test]
    fn trigger_rule_budgets_preserve_mixed_actions() {
        let mut rule = spec(vec![TriggerAction::Highlight; MAX_ACTIONS]);
        assert!(Trigger::compile(rule.clone()).is_ok());
        rule.actions.push(TriggerAction::Highlight);
        assert!(Trigger::compile(rule).is_err());

        let mut rule = spec(vec![TriggerAction::Execute {
            program: "helper".into(),
            args: vec![String::new()],
        }]);
        let remaining = MAX_RULE_BYTES - rule.id.len() - rule.regex.len() - "helper".len();
        if let TriggerAction::Execute { args, .. } = &mut rule.actions[0] {
            args[0] = "x".repeat(remaining);
        }
        assert!(Trigger::compile(rule.clone()).is_ok());
        if let TriggerAction::Execute { args, .. } = &mut rule.actions[0] {
            args[0].push('x');
        }
        assert!(Trigger::compile(rule).is_err());

        let mut trigger = Trigger::compile(spec(vec![
            TriggerAction::Highlight,
            TriggerAction::Respond {
                data: Some("ACK".into()),
                hex: None,
            },
            TriggerAction::Execute {
                program: "helper".into(),
                args: vec!["argument".into()],
            },
        ]))
        .unwrap();
        let hit = trigger.try_fire(&frame(Direction::Rx, "go")).unwrap();
        assert_eq!(hit.responses, vec![b"ACK".to_vec()]);
        assert_eq!(
            hit.commands,
            vec![("helper".into(), vec!["argument".into()])]
        );
    }

    #[test]
    fn anchored_matches_use_raw_and_decoded_text_independently() {
        let raw = frame(Direction::Rx, "READY");
        let decoded = raw
            .clone()
            .with_decoded(Some(crate::frame::DecodedInfo::info(
                "test",
                "START".into(),
                vec![],
            )));
        for (regex, input, expected) in [
            ("^READY$", &raw, Some("READY")),
            ("^READY$", &decoded, Some("READY")),
            ("^START$", &decoded, Some("START")),
            ("READY START", &decoded, None),
            ("READY $", &raw, None),
            ("READY|START", &decoded, Some("READY")),
        ] {
            let mut trigger = Trigger::compile(TriggerSpec {
                id: "anchored".into(),
                direction: Direction::Rx,
                regex: regex.into(),
                actions: vec![TriggerAction::Respond {
                    data: Some("ACK".into()),
                    hex: None,
                }],
                min_interval_ms: 0,
            })
            .unwrap();
            let hits = evaluate(std::slice::from_mut(&mut trigger), input);
            assert_eq!(
                hits.first().map(|hit| hit.matched.as_str()),
                expected,
                "{regex}"
            );
            assert_eq!(hits.len(), usize::from(expected.is_some()), "{regex}");
            if let Some(hit) = hits.first() {
                assert_eq!(hit.responses, vec![b"ACK".to_vec()]);
            }
        }
    }

    #[test]
    fn 触发_应答文本与十六进制() {
        let spec = TriggerSpec {
            id: "boot".into(),
            direction: Direction::Rx,
            regex: "READY".into(),
            actions: vec![TriggerAction::Respond {
                data: Some("START\r\n".into()),
                hex: None,
            }],
            min_interval_ms: 0,
        };
        let t = Trigger::compile(spec).unwrap();
        let hits = evaluate(&mut [t], &frame(Direction::Rx, "SYSTEM READY"));
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].responses, vec![b"START\r\n".to_vec()]);
        assert_eq!(hits[0].matched, "READY");
    }

    #[test]
    fn 触发_匹配方向与速率限制() {
        let spec = TriggerSpec {
            id: "x".into(),
            direction: Direction::Rx,
            regex: "ERR".into(),
            actions: vec![TriggerAction::Respond {
                data: None,
                hex: Some("4F4B".into()),
            }],
            min_interval_ms: 1_000,
        };
        let t = Trigger::compile(spec).unwrap();
        let mut triggers = [t];
        // Direction mismatch.
        let hits = evaluate(&mut triggers, &frame(Direction::Tx, "ERR"));
        assert!(hits.is_empty(), "{:?}", hits.is_empty());
        // First match.
        let hits = evaluate(&mut triggers, &frame(Direction::Rx, "ERR 1"));
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].responses, vec![vec![0x4F, 0x4B]]);
        // An immediate second match is rate-limited.
        let hits = evaluate(&mut triggers, &frame(Direction::Rx, "ERR 2"));
        assert!(hits.is_empty(), "{:?}", hits.is_empty());
    }

    #[test]
    fn 非法正则报错() {
        let spec = TriggerSpec {
            id: "bad".into(),
            direction: Direction::Rx,
            regex: "(oops".into(),
            actions: vec![],
            min_interval_ms: 100,
        };
        assert!(Trigger::compile(spec).is_err());
    }
}
