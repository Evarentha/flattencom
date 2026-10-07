/*
 * flattencom - Core Filter
 *
 * Compiles directional, text, time and decoded-field filters without altering captured data.
 *
 * Authors:
 * worryzu <worryzu@gmail.com> @LinearTeam
 *
 * Copyright (C) 2026 Evarentha
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

//! Server-side read filtering applies to views: `read_frames`, events and exports.
//! Recording, triggers and statistics always observe the unfiltered stream.

use regex::{Regex, RegexBuilder};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::FlattenError;
use crate::frame::{Direction, Frame};

/// A condition on one decoded field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FieldFilter {
    /// Field name, for example addr or fc; substring match.
    pub name: String,
    /// Substring required in the field value.
    pub contains: String,
}

/// Read filter; all specified conditions must match.
///
/// Matches visible frame text and decoded fields.
/// Control characters use their display representation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FilterSpec {
    /// Only include this direction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direction: Option<Direction>,
    /// Substring; regex takes precedence when both are supplied.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contains: Option<String>,
    /// Regular expression, taking precedence over substring.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub regex: Option<String>,
    /// Case-insensitive matching (default true).
    #[serde(default = "default_true")]
    pub case_insensitive: bool,
    /// Decoded field conditions; all must match.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<FieldFilter>,
    /// Inclusive lower time bound in UNIX microseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_us: Option<i64>,
    /// Exclusive upper time bound in UNIX microseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_us: Option<i64>,
    /// All nested conditions must match.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub all_of: Vec<FilterSpec>,
    /// At least one nested condition must match; an empty list adds no restriction.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub any_of: Vec<FilterSpec>,
}

impl Default for FilterSpec {
    fn default() -> Self {
        Self {
            direction: None,
            contains: None,
            regex: None,
            case_insensitive: default_true(),
            fields: Vec::new(),
            from_us: None,
            to_us: None,
            all_of: Vec::new(),
            any_of: Vec::new(),
        }
    }
}

fn default_true() -> bool {
    true
}

impl FilterSpec {
    /// Whether the filter is empty and matches everything.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.direction.is_none()
            && self.contains.is_none()
            && self.regex.is_none()
            && self.fields.is_empty()
            && self.from_us.is_none()
            && self.to_us.is_none()
            && self.all_of.is_empty()
            && self.any_of.is_empty()
    }
}

/// Compiled filter.
#[derive(Debug, Clone)]
pub struct CompiledFilter {
    direction: Option<Direction>,
    contains: Option<String>,
    regex: Option<Regex>,
    fields: Vec<(String, String)>,
    ci: bool,
    from_us: Option<i64>,
    to_us: Option<i64>,
    all_of: Vec<CompiledFilter>,
    any_of: Vec<CompiledFilter>,
}

impl CompiledFilter {
    /// Compile the filter and report invalid regular expressions.
    pub fn compile(spec: &FilterSpec) -> Result<Self, FlattenError> {
        Self::compile_depth(spec, 0)
    }

    fn compile_depth(spec: &FilterSpec, depth: usize) -> Result<Self, FlattenError> {
        if depth > 16 || spec.all_of.len() + spec.any_of.len() > 64 {
            return Err(FlattenError::InvalidConfig {
                field: "filter".into(),
                reason: "maximum depth 16; maximum children 64".into(),
            });
        }
        let bad = |reason: String| FlattenError::InvalidConfig {
            field: "filter.regex".into(),
            reason,
        };
        let regex = match &spec.regex {
            Some(p) if !p.is_empty() => Some(
                RegexBuilder::new(p)
                    .case_insensitive(spec.case_insensitive)
                    .build()
                    .map_err(|e| bad(e.to_string()))?,
            ),
            _ => None,
        };
        let contains = match (&regex, &spec.contains) {
            (None, Some(c)) if !c.is_empty() => Some(if spec.case_insensitive {
                c.to_lowercase()
            } else {
                c.clone()
            }),
            _ => None,
        };
        Ok(Self {
            direction: spec.direction,
            contains,
            regex,
            fields: spec
                .fields
                .iter()
                .map(|f| {
                    if spec.case_insensitive {
                        (f.name.to_lowercase(), f.contains.to_lowercase())
                    } else {
                        (f.name.clone(), f.contains.clone())
                    }
                })
                .collect(),
            ci: spec.case_insensitive,
            from_us: spec.from_us,
            to_us: spec.to_us,
            all_of: spec
                .all_of
                .iter()
                .map(|s| Self::compile_depth(s, depth + 1))
                .collect::<Result<_, _>>()?,
            any_of: spec
                .any_of
                .iter()
                .map(|s| Self::compile_depth(s, depth + 1))
                .collect::<Result<_, _>>()?,
        })
    }

    /// Match a frame.
    #[must_use]
    pub fn matches(&self, frame: &Frame) -> bool {
        if self.from_us.is_some_and(|t| frame.t_us < t)
            || self.to_us.is_some_and(|t| frame.t_us >= t)
            || !self.all_of.iter().all(|f| f.matches(frame))
            || (!self.any_of.is_empty() && !self.any_of.iter().any(|f| f.matches(frame)))
        {
            return false;
        }
        if self.direction.is_some_and(|d| frame.dir != d) {
            return false;
        }
        if self.regex.is_some() || self.contains.is_some() {
            let text = frame.text_visual();
            if let Some(re) = &self.regex {
                if !re.is_match(&text) {
                    return false;
                }
            } else if let Some(c) = &self.contains {
                let hit = if self.ci {
                    text.to_lowercase().contains(c)
                } else {
                    text.contains(c)
                };
                if !hit {
                    return false;
                }
            }
        }
        if !self.fields.is_empty() {
            let Some(decoded) = &frame.decoded else {
                return false;
            };
            for (name, want) in &self.fields {
                let hit = decoded.fields.iter().any(|f| {
                    if self.ci {
                        f.name.to_lowercase().contains(name)
                            && f.value.to_lowercase().contains(want)
                    } else {
                        f.name.contains(name) && f.value.contains(want)
                    }
                });
                if !hit {
                    return false;
                }
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fx(dir: Direction, text: &str, fields: Vec<(&str, &str)>) -> Frame {
        let fields = fields
            .into_iter()
            .map(|(n, v)| crate::frame::DecodeField {
                name: n.into(),
                value: v.into(),
            })
            .collect::<Vec<crate::frame::DecodeField>>();
        let mut f = Frame::new(0, dir, text.as_bytes().to_vec(), 0, 0);
        if !fields.is_empty() {
            f = f.with_decoded(Some(crate::frame::DecodedInfo::info(
                "modbus_rtu",
                text.into(),
                fields,
            )));
        }
        f
    }

    #[test]
    fn 方向过滤() {
        let spec = FilterSpec {
            direction: Some(Direction::Rx),
            ..Default::default()
        };
        let f = CompiledFilter::compile(&spec).unwrap();
        assert!(f.matches(&fx(Direction::Rx, "x", vec![])));
        assert!(!f.matches(&fx(Direction::Tx, "x", vec![])));
    }

    #[test]
    fn 子串与大小写() {
        let spec = FilterSpec {
            contains: Some("error".into()),
            ..Default::default()
        };
        let f = CompiledFilter::compile(&spec).unwrap();
        assert!(f.matches(&fx(Direction::Rx, "Boot ERROR here", vec![])));
        assert!(!f.matches(&fx(Direction::Rx, "boot ok", vec![])));
        let cs = FilterSpec {
            contains: Some("ERROR".into()),
            case_insensitive: false,
            ..Default::default()
        };
        let f = CompiledFilter::compile(&cs).unwrap();
        assert!(!f.matches(&fx(Direction::Rx, "error here", vec![])));
        assert!(f.matches(&fx(Direction::Rx, "ERROR here", vec![])));
    }

    #[test]
    fn 正则过滤() {
        let spec = FilterSpec {
            regex: Some("^seq=\\d+ lvl=0x1F".into()),
            ..Default::default()
        };
        let f = CompiledFilter::compile(&spec).unwrap();
        assert!(f.matches(&fx(Direction::Rx, "seq=42 lvl=0x1F", vec![])));
        assert!(!f.matches(&fx(Direction::Rx, "x seq=42 lvl=0x1F", vec![])));
    }

    #[test]
    fn 非法正则报错() {
        let spec = FilterSpec {
            regex: Some("(unclosed".into()),
            ..Default::default()
        };
        assert!(CompiledFilter::compile(&spec).is_err());
    }

    #[test]
    fn 字段过滤() {
        let spec = FilterSpec {
            fields: vec![FieldFilter {
                name: "fc".into(),
                contains: "Read holding".into(),
            }],
            ..Default::default()
        };
        let f = CompiledFilter::compile(&spec).unwrap();
        assert!(f.matches(&fx(
            Direction::Rx,
            "modbus",
            vec![("fc", "Read holding registers")]
        )));
        assert!(!f.matches(&fx(
            Direction::Rx,
            "modbus",
            vec![("fc", "Write single register")]
        )));
        assert!(!f.matches(&fx(Direction::Rx, "no decode", vec![])));
    }

    #[test]
    fn decoded_field_case_policy_applies_to_names_values_and_nested_filters() {
        for case_insensitive in [false, true] {
            let spec = FilterSpec {
                case_insensitive,
                fields: vec![FieldFilter {
                    name: "Status".into(),
                    contains: "OK".into(),
                }],
                ..Default::default()
            };
            // Nested nodes use their own case policy, independently of their parent.
            let all = FilterSpec {
                case_insensitive: !case_insensitive,
                all_of: vec![spec.clone()],
                ..Default::default()
            };
            let any = FilterSpec {
                case_insensitive: !case_insensitive,
                any_of: vec![spec.clone()],
                ..Default::default()
            };
            for spec in [spec, all, any] {
                let filter = CompiledFilter::compile(&spec).unwrap();
                for (name, value, exact) in [
                    ("DeviceStatus", "OKAY", true),
                    ("Devicestatus", "OKAY", false),
                    ("DeviceStatus", "okay", false),
                ] {
                    assert_eq!(
                        filter.matches(&fx(Direction::Rx, "", vec![(name, value)])),
                        exact || case_insensitive,
                        "name={name}, value={value}, case_insensitive={case_insensitive}"
                    );
                }
            }
        }
    }
}
