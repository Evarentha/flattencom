/*
 * flattencom - flattencom Flattencom-Proto Tests Fixtures
 *
 * Checks shared golden JSON-RPC examples against Rust frame and response types.
 *
 * Authors:
 * worryzu <worryzu@gmail.com> @LinearTeam
 *
 * Copyright (C) 2026 Evarentha
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

//! Golden wire examples shared with the Qt model tests.
use flattencom_proto::methods::{FramesPageOut, RpcResponse};

#[test]
fn frame_page_wire_contract() {
    let input = include_str!("../fixtures/frame-page.json");
    let response: RpcResponse = serde_json::from_str(input).unwrap();
    let page: FramesPageOut = serde_json::from_value(response.result).unwrap();
    assert_eq!(page.next_seq, 2);
    assert_eq!(page.frames[0].decoded_text.as_deref(), Some("AT␍␊"));
    assert_eq!(page.frames[1].text.as_deref(), Some("OK\r\n"));
    assert_eq!(
        page.frames.iter().map(|f| f.seq).collect::<Vec<_>>(),
        [0, 1]
    );
}
