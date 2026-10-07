/*
 * flattencom - flattencom Flattencom-Proto Src Framing
 *
 * Reads bounded UTF-8 NDJSON messages and rejects oversized or incomplete frames.
 *
 * Authors:
 * worryzu <worryzu@gmail.com> @LinearTeam
 *
 * Copyright (C) 2026 Evarentha
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

//! Bounded NDJSON framing shared by daemon and Rust clients.
use tokio::io::{AsyncBufRead, AsyncBufReadExt};

/// Read a complete UTF-8 line without buffering more than `limit` bytes.
/// `None` denotes clean EOF. A truncated final message is an error.
pub async fn read_line<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    limit: usize,
) -> std::io::Result<Option<String>> {
    let mut bytes = Vec::new();
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return if bytes.is_empty() {
                Ok(None)
            } else {
                Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "truncated NDJSON message",
                ))
            };
        }
        let end = available.iter().position(|b| *b == b'\n').map(|i| i + 1);
        let count = end.unwrap_or(available.len());
        if bytes.len().saturating_add(count) > limit {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "NDJSON message exceeds limit",
            ));
        }
        bytes.extend_from_slice(&available[..count]);
        reader.consume(count);
        if end.is_some() {
            return String::from_utf8(bytes)
                .map(Some)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn split_messages_and_limits() {
        let mut data = &b"{\"id\":1}\n{\"id\":2}\n"[..];
        assert_eq!(
            read_line(&mut data, 64).await.unwrap().unwrap(),
            "{\"id\":1}\n"
        );
        assert_eq!(
            read_line(&mut data, 64).await.unwrap().unwrap(),
            "{\"id\":2}\n"
        );
        assert!(read_line(&mut data, 64).await.unwrap().is_none());
        let mut data = &b"unterminated input"[..];
        assert_eq!(
            read_line(&mut data, 4).await.unwrap_err().kind(),
            std::io::ErrorKind::InvalidData
        );
        let mut data = &b"partial"[..];
        assert_eq!(
            read_line(&mut data, 64).await.unwrap_err().kind(),
            std::io::ErrorKind::UnexpectedEof
        );
    }
}
