/*
 * flattencom - Core Decode Wasm
 *
 * Hosts decoder modules with a bounded WebAssembly memory and instruction budget.
 *
 * Authors:
 * worryzu <worryzu@gmail.com> @LinearTeam
 *
 * Copyright (C) 2026 Evarentha
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

//! Fuel- and memory-limited WebAssembly decoder host. No WASI or host imports.
//! Linear memory is limited to 16 MiB; the single table may hold at most 65,536 elements.
//!
//! ABI v1 exports `memory`, `alloc(i32) -> i32`, and
//! `decode(i32, i32) -> i64`. Input is UTF-8 JSON (hex, dir, t_us, mono_us).
//! The result packs an output pointer into the high 32 bits and its length
//! into the low 32 bits. Output is a `DecodedInfo` JSON object, or `null`.
//! Guest state persists per direction; `alloc` owns input/output memory reuse.

use std::io::Read;

use wasmi::{
    Config, Engine, Linker, Memory, Module, Store, StoreLimits, StoreLimitsBuilder, TypedFunc,
};

use crate::FlattenError;
use crate::decode::{ChunkCtx, Decoder};
use crate::frame::DecodedInfo;

const MAX_MODULE_BYTES: usize = 16 * 1024 * 1024;
const MAX_TABLE_ELEMENTS: usize = 65_536;

fn read_module(reader: impl Read) -> Result<Vec<u8>, FlattenError> {
    let mut bytes = Vec::new();
    reader
        .take((MAX_MODULE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|e| FlattenError::Decode(e.to_string()))?;
    if bytes.len() > MAX_MODULE_BYTES {
        return Err(FlattenError::Decode("WASM module exceeds 16 MiB".into()));
    }
    Ok(bytes)
}

/// Sandboxed, deterministic decoder with a per-call instruction budget.
pub struct WasmDecoder {
    store: Store<StoreLimits>,
    memory: Memory,
    alloc: TypedFunc<i32, i32>,
    decode: TypedFunc<(i32, i32), i64>,
    fuel: u64,
    disabled: bool,
}

impl WasmDecoder {
    /// Load a `.wasm` or `.wat` module from `options.path`.
    ///
    /// # Errors
    /// Returns a decode error for IO, validation, unsupported imports or ABI mismatches.
    pub fn load(options: &serde_json::Value) -> Result<Self, FlattenError> {
        let path = options
            .get("path")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| FlattenError::Decode("wasm requires options.path".into()))?;
        let file = std::fs::File::open(path).map_err(|e| FlattenError::Decode(e.to_string()))?;
        let bytes = read_module(file)?;
        let fuel = options
            .get("fuel")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(1_000_000)
            .clamp(1000, 10_000_000);
        Self::from_bytes(&bytes, fuel)
    }

    fn from_bytes(bytes: &[u8], fuel: u64) -> Result<Self, FlattenError> {
        let mut config = Config::default();
        config.consume_fuel(true);
        let engine = Engine::new(&config);
        let module = Module::new(&engine, bytes).map_err(wasm_error)?;
        let limits = StoreLimitsBuilder::new()
            .memory_size(16 * 1024 * 1024)
            .table_elements(MAX_TABLE_ELEMENTS)
            .instances(1)
            .memories(1)
            .tables(1)
            .build();
        let mut store = Store::new(&engine, limits);
        store.limiter(|limits| limits);
        store.set_fuel(fuel).map_err(wasm_error)?;
        // An empty linker denies all imports, including filesystem/network/process IO.
        let instance = Linker::new(&engine)
            .instantiate_and_start(&mut store, &module)
            .map_err(wasm_error)?;
        let memory = instance
            .get_memory(&store, "memory")
            .ok_or_else(|| FlattenError::Decode("WASM must export memory".into()))?;
        let alloc = instance
            .get_typed_func(&store, "alloc")
            .map_err(wasm_error)?;
        let decode = instance
            .get_typed_func(&store, "decode")
            .map_err(wasm_error)?;
        Ok(Self {
            store,
            memory,
            alloc,
            decode,
            fuel,
            disabled: false,
        })
    }

    fn run(&mut self, ctx: &ChunkCtx<'_>) -> Result<Option<DecodedInfo>, FlattenError> {
        let input = serde_json::to_vec(&serde_json::json!({
            "hex": hex::encode_upper(ctx.data), "dir": ctx.dir.as_str(),
            "t_us": ctx.t_us, "mono_us": ctx.mono_us,
        }))
        .map_err(|e| FlattenError::Decode(e.to_string()))?;
        let len = i32::try_from(input.len()).map_err(|e| FlattenError::Decode(e.to_string()))?;
        self.store.set_fuel(self.fuel).map_err(wasm_error)?;
        let ptr = self.alloc.call(&mut self.store, len).map_err(wasm_error)?;
        let offset = usize::try_from(ptr).map_err(|e| FlattenError::Decode(e.to_string()))?;
        self.memory
            .write(&mut self.store, offset, &input)
            .map_err(|e| FlattenError::Decode(e.to_string()))?;
        let result = self
            .decode
            .call(&mut self.store, (ptr, len))
            .map_err(wasm_error)?;
        let result = u64::from_ne_bytes(result.to_ne_bytes());
        let output_len = (result & 0xffff_ffff) as usize;
        let output_ptr = (result >> 32) as usize;
        if output_len > 1024 * 1024 {
            return Err(FlattenError::Decode("WASM response exceeds 1 MiB".into()));
        }
        let mut output = vec![0; output_len];
        self.memory
            .read(&self.store, output_ptr, &mut output)
            .map_err(|e| FlattenError::Decode(e.to_string()))?;
        // Validate ignored JSON fields as well as those deserialized into DecodedInfo.
        let output =
            std::str::from_utf8(&output).map_err(|e| FlattenError::Decode(e.to_string()))?;
        serde_json::from_str(output).map_err(|e| FlattenError::Decode(e.to_string()))
    }
}

fn wasm_error(error: wasmi::Error) -> FlattenError {
    FlattenError::Decode(error.to_string())
}

impl Decoder for WasmDecoder {
    fn id(&self) -> &'static str {
        "wasm"
    }
    fn feed(&mut self, ctx: &ChunkCtx<'_>) -> Option<DecodedInfo> {
        if self.disabled {
            return None;
        }
        match self.run(ctx) {
            Ok(value) => value,
            Err(e) => {
                self.disabled = true;
                Some(DecodedInfo::error("wasm", e.to_string(), vec![]))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::{DecodeLevel, Direction};

    fn context() -> ChunkCtx<'static> {
        ChunkCtx {
            dir: Direction::Rx,
            t_us: 0,
            mono_us: 0,
            data: b"AT",
        }
    }

    #[test]
    fn module_read_accepts_limit_and_never_consumes_beyond_sentinel() {
        let exact = read_module(std::io::repeat(0).take(MAX_MODULE_BYTES as u64)).unwrap();
        assert_eq!(exact.len(), MAX_MODULE_BYTES);
        let mut endless = std::io::repeat(0).take(u64::MAX);
        let error = read_module(&mut endless).unwrap_err();
        assert_eq!(
            error,
            FlattenError::Decode("WASM module exceeds 16 MiB".into())
        );
        assert_eq!(u64::MAX - endless.limit(), (MAX_MODULE_BYTES + 1) as u64);
    }

    #[test]
    fn oversized_module_file_is_rejected_before_parsing() {
        let file = tempfile::NamedTempFile::new().unwrap();
        file.as_file()
            .set_len((MAX_MODULE_BYTES + 1) as u64)
            .unwrap();
        let result = WasmDecoder::load(&serde_json::json!({"path": file.path()}));
        assert!(
            matches!(result, Err(FlattenError::Decode(message)) if message == "WASM module exceeds 16 MiB")
        );
    }

    #[test]
    fn table_limits_cover_initial_allocation_and_persistent_growth() {
        let module = |initial| {
            format!(
                r#"(module
            (table $table {initial} funcref)
            (memory (export "memory") 1)
            (data (i32.const 0) "null")
            (func (export "alloc") (param i32) (result i32) i32.const 1024)
            (func (export "decode") (param i32 i32) (result i64)
                ref.null func i32.const 32768 table.grow $table drop
                table.size $table i32.const 65536 i32.gt_u
                if unreachable end
                i64.const 4))"#
            )
        };
        assert!(
            WasmDecoder::from_bytes(module(MAX_TABLE_ELEMENTS + 1).as_bytes(), 1_000_000).is_err()
        );
        assert!(WasmDecoder::from_bytes(module(MAX_TABLE_ELEMENTS).as_bytes(), 1_000_000).is_ok());
        let mut decoder = WasmDecoder::from_bytes(module(0).as_bytes(), 1_000_000).unwrap();
        // Each call receives fresh fuel. Growth must stop at the persistent table limit.
        for _ in 0..8 {
            assert!(decoder.feed(&context()).is_none());
            assert!(!decoder.disabled);
        }
    }

    #[test]
    fn actual_wasm_abi_and_instruction_budget() {
        let mut example =
            WasmDecoder::from_bytes(include_bytes!("../../../../examples/decoder.wat"), 10000)
                .unwrap();
        assert_eq!(example.feed(&context()).unwrap().text, "WASM decoded");
        let mut decoder = WasmDecoder::from_bytes(
            br#"(module
            (memory (export "memory") 1)
            (data (i32.const 0) "null")
            (func (export "alloc") (param i32) (result i32) i32.const 1024)
            (func (export "decode") (param i32 i32) (result i64) i64.const 4))"#,
            10000,
        )
        .unwrap();
        assert!(decoder.feed(&context()).is_none());
        assert!(!decoder.disabled);
        let mut runaway = WasmDecoder::from_bytes(
            br#"(module
            (memory (export "memory") 1)
            (func (export "alloc") (param i32) (result i32) i32.const 1024)
            (func (export "decode") (param i32 i32) (result i64)
                (loop $forever br $forever) i64.const 0))"#,
            10000,
        )
        .unwrap();
        assert_eq!(runaway.feed(&context()).unwrap().level, DecodeLevel::Error);
        assert!(runaway.feed(&context()).is_none());
    }

    #[test]
    fn invalid_utf8_in_unknown_output_fields_disables_guest() {
        let guest = |output: &[u8]| {
            use std::fmt::Write as _;
            let mut escaped = String::new();
            for byte in output {
                write!(escaped, "\\{byte:02x}").unwrap();
            }
            let module = format!(
                r#"(module
                (memory (export "memory") 1)
                (data (i32.const 0) "{escaped}")
                (func (export "alloc") (param i32) (result i32) i32.const 1024)
                (func (export "decode") (param i32 i32) (result i64) i64.const {}))"#,
                output.len()
            );
            WasmDecoder::from_bytes(module.as_bytes(), 10000).unwrap()
        };
        let mut invalid = guest(b"{\"decoder\":\"wasm\",\"text\":\"accepted\",\"extra\":\"\xff\"}");
        assert_eq!(invalid.feed(&context()).unwrap().level, DecodeLevel::Error);
        assert!(invalid.disabled);
        assert!(invalid.feed(&context()).is_none());

        let mut valid =
            guest("{\"decoder\":\"wasm\",\"text\":\"accepted\",\"extra\":\"�\"}".as_bytes());
        for _ in 0..2 {
            assert_eq!(valid.feed(&context()).unwrap().text, "accepted");
            assert!(!valid.disabled);
        }
    }
}
