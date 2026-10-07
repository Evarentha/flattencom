/*
 * flattencom - Core Decode Modbus Rtu
 *
 * Validates Modbus RTU CRCs and interprets requests, responses and exception fields.
 *
 * Authors:
 * worryzu <worryzu@gmail.com> @LinearTeam
 *
 * Copyright (C) 2026 Evarentha
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

//! Modbus RTU decoder: CRC16 framing, fields, readable exceptions and master/slave disambiguation.
//!
//! ## Framing strategy
//!
//! RTU is a byte stream; RX/TX alone cannot identify requests without knowing the capture role.
//! Default **auto mode** checks CRC16 at candidate lengths: exception `5`,
//! byte-count response `3+N+2`, and request `8`; consume the shortest valid candidate.
//!
//! Auto mode checks candidate boundaries, but CRC is not a reliable delimiter; specify
//! the known capture role. Damaged data with ambiguous boundaries waits for more bytes.
//! For a fixed capture point, use `{"role":"master"}` for the master side
//! (RX responses, TX requests), or `{"role":"slave"}` to remove ambiguity.
//!
//! Supported functions: 1/2/3/4 read, 5/6 single write, 15/16 multiple write, 17 server ID;
//! exception responses (`fc|0x80`) receive readable descriptions.

use serde::{Deserialize, Serialize};
use std::fmt::Write as _;

use crate::FlattenError;
use crate::decode::{ChunkCtx, Decoder};
use crate::frame::{DecodeField, DecodeLevel, DecodedInfo};

/// Master/slave role used for disambiguation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// Auto: infer interpretation from frame shape.
    #[default]
    Auto,
    /// Master-side capture: RX responses, TX requests.
    Master,
    /// Slave-side capture: RX requests, TX responses.
    Slave,
}

impl Role {
    /// Parse the role option.
    pub fn parse(s: &str) -> Result<Self, FlattenError> {
        match s {
            "auto" => Ok(Self::Auto),
            "master" => Ok(Self::Master),
            "slave" => Ok(Self::Slave),
            other => Err(FlattenError::InvalidConfig {
                field: "options.role".into(),
                reason: crate::tr!(
                    "Expected auto, master or slave, got {other:?}",
                    other = other
                ),
            }),
        }
    }
}

/// Modbus RTU decoder with an incomplete-frame buffer across chunks.
#[derive(Debug, Default)]
pub struct ModbusRtuDecoder {
    buf: Vec<u8>,
    role: Role,
}

/// Buffer limit: reject malformed streams rather than retaining unlimited partial frames.
const BUFFER_CAP: usize = 256;

impl ModbusRtuDecoder {
    /// Construct with master/slave/auto role; see module documentation.
    pub fn with_role(role: Role) -> Self {
        Self {
            buf: Vec::new(),
            role,
        }
    }
}

impl Decoder for ModbusRtuDecoder {
    fn id(&self) -> &'static str {
        "modbus_rtu"
    }

    fn feed(&mut self, ctx: &ChunkCtx<'_>) -> Option<DecodedInfo> {
        self.buf.extend_from_slice(ctx.data);
        let mut frames: Vec<Parsed> = Vec::new();
        let mut fatal: Option<String> = None;

        while self.buf.len() >= 2 {
            let interp = self.interpretation(ctx.dir, &self.buf);
            let candidates = candidate_lengths(&self.buf, interp);
            let valid = candidates.iter().copied().find(|&len| {
                self.buf.len() >= len
                    && crc16(&self.buf[..len - 2])
                        == u16::from_le_bytes([self.buf[len - 2], self.buf[len - 1]])
            });
            // Only consume a bad CRC when all known length candidates are available.
            // In particular an 11-byte response must not be consumed as an 8-byte request.
            let complete_bad = candidates
                .last()
                .copied()
                .filter(|len| *len <= self.buf.len());
            if let Some(len) = valid.or(complete_bad) {
                let wire = &self.buf[..len];
                let body = &wire[..len - 2];
                let actual_crc = u16::from_le_bytes([wire[len - 2], wire[len - 1]]);
                let crc_ok = crc16(body) == actual_crc;
                frames.push(Parsed {
                    body: body.to_vec(),
                    actual_crc,
                    crc_ok,
                    interp,
                });
                self.buf.drain(..len);
                continue;
            }
            // No candidate is complete: wait for bytes; reject input exceeding the buffer limit.
            if self.buf.len() > BUFFER_CAP {
                fatal = Some(crate::tr!(
                    "No frame found within {BUFFER_CAP} bytes; buffered data discarded",
                    BUFFER_CAP = BUFFER_CAP
                ));
                self.buf.clear();
            }
            break;
        }

        if frames.is_empty() && fatal.is_none() {
            return None;
        }
        let mut text = String::new();
        let mut fields: Vec<DecodeField> = Vec::new();
        let mut level = DecodeLevel::Info;
        for (i, f) in frames.iter().enumerate() {
            let info = decode_frame(f);
            if i == 0 {
                fields.clone_from(&info.fields);
            } else {
                fields.clear(); // Multiple frames are combined; clear ambiguous per-frame fields.
            }
            if info.level == DecodeLevel::Error {
                level = DecodeLevel::Error;
            }
            text.push_str(&info.text);
            text.push('\n');
        }
        if let Some(msg) = fatal {
            level = DecodeLevel::Error;
            writeln!(text, "{}", crate::tr!("Warning: {msg}", msg = msg)).expect("String write");
        }
        text.pop();
        if frames.len() > 1 {
            fields.push(DecodeField {
                name: "frames".into(),
                value: frames.len().to_string(),
            });
        }
        Some(DecodedInfo {
            decoder: "modbus_rtu".into(),
            level,
            text,
            fields,
        })
    }
}

impl ModbusRtuDecoder {
    /// Infer interpretation from role and direction when lengths are ambiguous.
    fn interpretation(&self, dir: crate::frame::Direction, _body: &[u8]) -> Interp {
        match self.role {
            Role::Auto => Interp::Auto,
            Role::Master => {
                if dir == crate::frame::Direction::Rx {
                    Interp::Response
                } else {
                    Interp::Request
                }
            }
            Role::Slave => {
                if dir == crate::frame::Direction::Rx {
                    Interp::Request
                } else {
                    Interp::Response
                }
            }
        }
    }
}

/// Interpretation hint for equal-length candidates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Interp {
    Auto,
    Request,
    Response,
}

struct Parsed {
    body: Vec<u8>,
    actual_crc: u16,
    crc_ok: bool,
    interp: Interp,
}

/// Candidate frame lengths; CRC checks determine which candidate is consumed.
fn candidate_lengths(buf: &[u8], interp: Interp) -> Vec<usize> {
    let mut out = Vec::with_capacity(3);
    let fc = buf[1];
    // Exception response: addr fc|0x80 code crc = 5 bytes.
    match fc {
        0x81..=0xff => out.push(5),
        1..=4 => {
            if interp != Interp::Response {
                out.push(8);
            }
            if interp != Interp::Request && buf.len() >= 3 && buf[2] > 0 && buf[2] <= 250 {
                out.push(5 + usize::from(buf[2]));
            }
        }
        5 | 6 => out.push(8),
        15 | 16 => {
            if interp != Interp::Request {
                out.push(8);
            }
            if interp != Interp::Response && buf.len() >= 7 && buf[6] <= 246 {
                out.push(9 + usize::from(buf[6]));
            }
        }
        17 => {
            if interp != Interp::Response {
                out.push(4);
            }
            if interp != Interp::Request && buf.len() >= 3 && buf[2] <= 250 {
                out.push(5 + usize::from(buf[2]));
            }
        }
        _ => {}
    }
    out.sort_unstable();
    out.dedup();
    out
}

/// Modbus CRC-16: polynomial 0xA001, initial 0xFFFF, low byte first.
#[must_use]
pub fn crc16(data: &[u8]) -> u16 {
    let mut crc: u16 = 0xFFFF;
    for &b in data {
        crc ^= u16::from(b);
        for _ in 0..8 {
            if crc & 1 != 0 {
                crc = (crc >> 1) ^ 0xA001;
            } else {
                crc >>= 1;
            }
        }
    }
    crc
}

fn fc_name(fc: u8) -> &'static str {
    match fc {
        1 => "Read coils",
        2 => "Read discrete inputs",
        3 => "Read holding registers",
        4 => "Read input registers",
        5 => "Write single coil",
        6 => "Write single register",
        15 => "Write multiple coils",
        16 => "Write multiple registers",
        17 => "Report server ID",
        _ => "Unknown function code",
    }
}

fn exception_name(code: u8) -> &'static str {
    match code {
        1 => "Illegal function",
        2 => "Illegal data address",
        3 => "Illegal data value",
        4 => "Server device failure",
        5 => "Acknowledged (operation in progress)",
        6 => "Server device busy",
        8 => "Checksum error",
        10 => "Gateway path unavailable",
        11 => "Gateway target failed to respond",
        _ => "Unknown exception",
    }
}

fn decode_frame(f: &Parsed) -> DecodedInfo {
    let b = &f.body;
    let addr = b[0];
    let fc = b[1];
    let mut fields = vec![
        DecodeField {
            name: "addr".into(),
            value: format!("{addr:02X}"),
        },
        DecodeField {
            name: "fc".into(),
            value: format!("{fc} {name}", name = fc_name(fc & 0x7F)),
        },
    ];
    let mut level = DecodeLevel::Info;

    if !f.crc_ok {
        level = DecodeLevel::Error;
        let expect = crc16(b);
        fields.push(DecodeField {
            name: "crc".into(),
            value: crate::tr!(
                "BAD (expected {:04X}, got {:04X})",
                expect.swap_bytes(),
                f.actual_crc.swap_bytes()
            ),
        });
        return DecodedInfo {
            decoder: "modbus_rtu".into(),
            level,
            text: crate::tr!(
                "Server {addr:02X} function {fc}: CRC mismatch (expected {:04X}, got {:04X})",
                expect.swap_bytes(),
                f.actual_crc.swap_bytes(),
                addr = addr,
                fc = fc
            ),
            fields,
        };
    }

    let text = if fc & 0x80 != 0 {
        // Exception response
        let code = b[2];
        level = DecodeLevel::Error;
        fields.push(DecodeField {
            name: "exception".into(),
            value: format!("{code} {name}", name = exception_name(code)),
        });
        crate::tr!(
            "Server {addr:02X} exception: {name}, code {code}",
            name = exception_name(code),
            addr = addr,
            code = code
        )
    } else if fc == 17 {
        if b.len() == 2 && f.interp != Interp::Response {
            crate::tr!("Server {addr:02X} Report server ID request", addr = addr)
        } else if f.interp != Interp::Request && b.len() >= 5 && b.len() == 3 + usize::from(b[2]) {
            let server_id = b[3];
            let status = b[4];
            let run_status = match status {
                0x00 => "OFF",
                0xff => "ON",
                _ => {
                    level = DecodeLevel::Error;
                    "INVALID"
                }
            };
            let additional = hex::encode_upper(&b[5..]);
            fields.extend([
                DecodeField {
                    name: "bytes".into(),
                    value: b[2].to_string(),
                },
                DecodeField {
                    name: "server_id".into(),
                    value: format!("0x{server_id:02X}"),
                },
                DecodeField {
                    name: "run_status".into(),
                    value: format!("{run_status} (0x{status:02X})"),
                },
                DecodeField {
                    name: "additional_data".into(),
                    value: additional.clone(),
                },
            ]);
            crate::tr!(
                "Server {addr:02X} Report server ID response: server_id=0x{server_id:02X} run_status={run_status} (0x{status:02X}) additional_data={additional}",
                addr = addr,
                server_id = server_id,
                run_status = run_status,
                status = status,
                additional = additional
            )
        } else {
            level = DecodeLevel::Error;
            crate::tr!(
                "Server {addr:02X} Report server ID: invalid frame length or byte count",
                addr = addr
            )
        }
    } else if (1..=4).contains(&fc)
        && b.len() >= 3
        && b.len() == 3 + b[2] as usize
        && f.interp != Interp::Request
    {
        // Read response with byte count; interpret as request when interp=Request.
        let n = b[2] as usize;
        if matches!(fc, 3 | 4) && !n.is_multiple_of(2) {
            level = DecodeLevel::Error;
        }
        let vals: Vec<String> = if matches!(fc, 1 | 2) {
            b[3..3 + n].iter().map(|b| format!("{b:02X}")).collect()
        } else {
            b[3..3 + n].chunks(2).map(hex::encode_upper).collect()
        };
        fields.push(DecodeField {
            name: "bytes".into(),
            value: n.to_string(),
        });
        fields.push(DecodeField {
            name: "values".into(),
            value: vals.join(" "),
        });
        crate::tr!(
            "Server {addr:02X} {name} response: {vals}",
            name = fc_name(fc),
            vals = vals.join(" "),
            addr = addr
        )
    } else if (1..=4).contains(&fc) && b.len() >= 6 {
        // Read request: addr fc start count crc (6-byte body, 8 bytes on the wire).
        let start = u16::from_be_bytes([b[2], b[3]]);
        let count = u16::from_be_bytes([b[4], b[5]]);
        fields.push(DecodeField {
            name: "start".into(),
            value: format!("0x{start:04X}"),
        });
        fields.push(DecodeField {
            name: "count".into(),
            value: count.to_string(),
        });
        crate::tr!(
            "Server {addr:02X} {name} request: start=0x{start:04X} count={count}",
            name = fc_name(fc),
            addr = addr,
            count = count,
            start = start
        )
    } else if b.len() >= 6 {
        // Write request/echo response: addr fc addr value crc (6-byte body, 8 bytes on the wire).
        let start = u16::from_be_bytes([b[2], b[3]]);
        let val = u16::from_be_bytes([b[4], b[5]]);
        fields.push(DecodeField {
            name: "address".into(),
            value: format!("0x{start:04X}"),
        });
        fields.push(DecodeField {
            name: "value".into(),
            value: format!("0x{val:04X}"),
        });
        crate::tr!(
            "Server {addr:02X} {name}: address=0x{start:04X} value=0x{val:04X}",
            name = fc_name(fc),
            addr = addr,
            start = start,
            val = val
        )
    } else {
        fields.push(DecodeField {
            name: "len".into(),
            value: b.len().to_string(),
        });
        crate::tr!(
            "Server {addr:02X} function {fc} (short frame: {} bytes)",
            b.len(),
            addr = addr,
            fc = fc
        )
    };
    fields.push(DecodeField {
        name: "crc".into(),
        value: "OK".into(),
    });
    DecodedInfo {
        decoder: "modbus_rtu".into(),
        level,
        text,
        fields,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::Direction;

    fn ctx(data: &[u8]) -> ChunkCtx<'_> {
        ChunkCtx {
            dir: Direction::Rx,
            t_us: 0,
            mono_us: 0,
            data,
        }
    }

    fn frame_with_crc(body: &[u8]) -> Vec<u8> {
        let mut v = body.to_vec();
        let crc = crc16(body);
        v.extend_from_slice(&crc.to_le_bytes());
        v
    }

    #[test]
    fn crc16_已知向量() {
        // Standard check value: CRC-16/MODBUS of "123456789" is 0x4B37.
        assert_eq!(crc16(b"123456789"), 0x4B37);
        // Reference vector: 01 03 00 00 00 01 gives 0x0A84 (wire bytes 84 0A).
        assert_eq!(crc16(&[0x01, 0x03, 0x00, 0x00, 0x00, 0x01]), 0x0A84);
        // Reference vector: 11 03 00 6B 00 03 gives 0x8776 (wire bytes 76 87).
        assert_eq!(crc16(&[0x11, 0x03, 0x00, 0x6B, 0x00, 0x03]), 0x8776);
    }

    #[test]
    fn 解码读保持寄存器请求() {
        let bytes = frame_with_crc(&[0x01, 0x03, 0x00, 0x00, 0x00, 0x02]);
        let mut d = ModbusRtuDecoder::default();
        let info = d.feed(&ctx(&bytes)).unwrap();
        assert_eq!(info.level, DecodeLevel::Info);
        assert!(
            info.text.contains("Read holding registers request"),
            "{}",
            info.text
        );
        assert!(info.text.contains("count=2"));
        assert!(
            info.fields
                .iter()
                .any(|f| f.name == "crc" && f.value == "OK")
        );
    }

    #[test]
    fn 解码读保持寄存器响应() {
        // 01 03 02 00 0A + CRC
        let bytes = frame_with_crc(&[0x01, 0x03, 0x02, 0x00, 0x0A]);
        let mut d = ModbusRtuDecoder::default();
        let info = d.feed(&ctx(&bytes)).unwrap();
        assert!(info.text.contains("response"), "{}", info.text);
        assert!(
            info.fields
                .iter()
                .any(|f| f.name == "values" && f.value == "000A")
        );
    }

    #[test]
    fn 解码异常响应() {
        // 01 83 02 plus CRC: slave 01, function 03 exception, code 02 illegal address.
        let bytes = frame_with_crc(&[0x01, 0x83, 0x02]);
        let mut d = ModbusRtuDecoder::default();
        let info = d.feed(&ctx(&bytes)).unwrap();
        assert_eq!(info.level, DecodeLevel::Error);
        assert!(info.text.contains("Illegal data address"));
    }

    #[test]
    fn crc_坏帧报错() {
        let mut bytes = frame_with_crc(&[0x01, 0x03, 0x00, 0x00, 0x00, 0x02]);
        bytes[3] ^= 0xFF; // Corrupt the data.
        let mut d = ModbusRtuDecoder::default();
        let info = d.feed(&ctx(&bytes)).unwrap();
        assert_eq!(info.level, DecodeLevel::Error);
        assert!(info.text.contains("CRC mismatch"), "{}", info.text);
    }

    #[test]
    fn 跨帧拆分仍可解码() {
        let bytes = frame_with_crc(&[0x01, 0x03, 0x00, 0x00, 0x00, 0x02]);
        let mut d = ModbusRtuDecoder::default();
        assert!(d.feed(&ctx(&bytes[..4])).is_none()); // Partial frame: no result.
        let info = d.feed(&ctx(&bytes[4..])).unwrap(); // Complete the frame.
        assert!(info.text.contains("Read holding registers"));
    }

    #[test]
    fn 多寄存器响应不能被短请求候选截断() {
        let bytes = frame_with_crc(&[1, 3, 6, 0, 1, 0, 2, 0, 3]);
        let mut decoder = ModbusRtuDecoder::default();
        assert!(decoder.feed(&ctx(&bytes[..8])).is_none());
        let result = decoder.feed(&ctx(&bytes[8..])).unwrap();
        assert_eq!(result.level, DecodeLevel::Info);
        assert!(
            result
                .fields
                .iter()
                .any(|f| f.name == "values" && f.value == "0001 0002 0003")
        );
    }

    #[test]
    fn 指定主站解析写多个寄存器请求() {
        let bytes = frame_with_crc(&[1, 16, 0, 0, 0, 2, 4, 0, 10, 0, 11]);
        let mut decoder = ModbusRtuDecoder::with_role(Role::Master);
        let result = decoder
            .feed(&ChunkCtx {
                dir: Direction::Tx,
                t_us: 0,
                mono_us: 0,
                data: &bytes,
            })
            .unwrap();
        assert_eq!(result.level, DecodeLevel::Info);
        assert!(
            result
                .fields
                .iter()
                .any(|f| f.name == "crc" && f.value == "OK")
        );
    }

    #[test]
    fn 双帧合并输出() {
        let b1 = frame_with_crc(&[0x01, 0x03, 0x00, 0x00, 0x00, 0x02]);
        let b2 = frame_with_crc(&[0x01, 0x06, 0x00, 0x0A, 0x12, 0x34]);
        let mut all = b1;
        all.extend_from_slice(&b2);
        let mut d = ModbusRtuDecoder::default();
        let info = d.feed(&ctx(&all)).unwrap();
        assert!(info.text.lines().count() == 2, "两行:{}", info.text);
        assert!(
            info.fields
                .iter()
                .any(|f| f.name == "frames" && f.value == "2")
        );
    }

    #[test]
    fn master_角色消歧请求响应() {
        // Equal-length ambiguity: an 8-byte wire frame (6-byte body) with fc=3 and b[2]=3
        // can mean response N=3 or request start=0x03XX; master RX is a response, master TX a request.
        let wire = frame_with_crc(&[0x01, 0x03, 0x03, 0x00, 0x0A, 0x0B]);
        let mut d = ModbusRtuDecoder::with_role(Role::Master);
        let rx_ctx = ChunkCtx {
            dir: Direction::Rx,
            t_us: 0,
            mono_us: 0,
            data: &wire,
        };
        let info = d.feed(&rx_ctx).unwrap();
        assert!(
            info.text.contains("response"),
            "master RX 应解释为响应:{}",
            info.text
        );
        let mut d = ModbusRtuDecoder::with_role(Role::Master);
        let tx_ctx = ChunkCtx {
            dir: Direction::Tx,
            t_us: 0,
            mono_us: 0,
            data: &wire,
        };
        let info = d.feed(&tx_ctx).unwrap();
        assert!(
            info.text.contains("request"),
            "master TX 应解释为请求:{}",
            info.text
        );
    }

    #[test]
    fn server_id_requests_and_responses_follow_capture_role() {
        let request = frame_with_crc(&[1, 17]);
        let response = frame_with_crc(&[1, 17, 4, 42, 255, 65, 66]);
        for (role, request_dir, response_dir) in [
            (Role::Master, Direction::Tx, Direction::Rx),
            (Role::Slave, Direction::Rx, Direction::Tx),
            (Role::Auto, Direction::Rx, Direction::Rx),
        ] {
            for (wire, dir, is_request) in [
                (&request, request_dir, true),
                (&response, response_dir, false),
            ] {
                for split in 0..wire.len() {
                    let mut decoder = ModbusRtuDecoder::with_role(role);
                    assert!(
                        decoder
                            .feed(&ChunkCtx {
                                dir,
                                ..ctx(&wire[..split])
                            })
                            .is_none()
                    );
                    let info = decoder
                        .feed(&ChunkCtx {
                            dir,
                            ..ctx(&wire[split..])
                        })
                        .unwrap();
                    assert_eq!(info.level, DecodeLevel::Info);
                    assert!(
                        !info
                            .fields
                            .iter()
                            .any(|f| matches!(f.name.as_str(), "address" | "value"))
                    );
                    if is_request {
                        assert!(info.text.ends_with("request"));
                    } else {
                        assert!(info.text.contains("response"));
                        for (name, value) in [
                            ("server_id", "0x2A"),
                            ("run_status", "ON (0xFF)"),
                            ("additional_data", "4142"),
                        ] {
                            assert!(
                                info.fields
                                    .iter()
                                    .any(|f| f.name == name && f.value == value)
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn server_id_response_requires_id_and_valid_run_status() {
        for body in [vec![1, 17, 0], vec![1, 17, 1, 42], vec![1, 17, 2, 42, 1]] {
            let wire = frame_with_crc(&body);
            let mut decoder = ModbusRtuDecoder::with_role(Role::Master);
            let info = decoder.feed(&ctx(&wire)).unwrap();
            assert_eq!(info.level, DecodeLevel::Error);
            assert!(
                info.fields
                    .iter()
                    .any(|f| f.name == "crc" && f.value == "OK")
            );
        }
        let wire = frame_with_crc(&[1, 17, 2, 42, 0]);
        let info = ModbusRtuDecoder::with_role(Role::Master)
            .feed(&ctx(&wire))
            .unwrap();
        assert_eq!(info.level, DecodeLevel::Info);
        assert!(
            info.fields
                .iter()
                .any(|f| f.name == "run_status" && f.value == "OFF (0x00)")
        );
        assert!(
            info.fields
                .iter()
                .any(|f| f.name == "additional_data" && f.value.is_empty())
        );
        let malformed = Parsed {
            body: vec![1, 17, 3, 42, 255],
            actual_crc: 0,
            crc_ok: true,
            interp: Interp::Response,
        };
        assert_eq!(decode_frame(&malformed).level, DecodeLevel::Error);
    }

    #[test]
    fn server_id_chinese_descriptions_preserve_structured_fields() {
        use crate::i18n::{Language, with_language};

        for body in [
            vec![1, 17],
            vec![1, 17, 4, 42, 255, 65, 66],
            vec![1, 17, 1, 42],
        ] {
            let wire = frame_with_crc(&body);
            let decode = || {
                let role = if body.len() == 2 {
                    Role::Slave
                } else {
                    Role::Master
                };
                ModbusRtuDecoder::with_role(role).feed(&ctx(&wire)).unwrap()
            };
            let english = with_language(Language::English, decode);
            let chinese = with_language(Language::Chinese, decode);
            assert_ne!(chinese.text, english.text);
            assert!(
                chinese
                    .text
                    .chars()
                    .any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c))
            );
            assert!(chinese.text.contains("01"));
            assert_eq!(chinese.decoder, english.decoder);
            assert_eq!(chinese.level, english.level);
            assert_eq!(chinese.fields, english.fields);
            if body.len() == 7 {
                for value in ["0x2A", "ON", "0xFF", "4142"] {
                    assert!(chinese.text.contains(value), "{}", chinese.text);
                }
            }
        }
    }

    #[test]
    fn 角色_选项解析() {
        assert_eq!(Role::parse("master").unwrap(), Role::Master);
        assert!(Role::parse("gateway").is_err());
    }
}
