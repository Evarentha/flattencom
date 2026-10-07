;; flattencom - WebAssembly Decoder Example
;;
;; Implements the decoder ABI exports and returns a sample decoded JSON result.
;;
;; Authors:
;; worryzu <worryzu@gmail.com> @LinearTeam
;;
;; Copyright (C) 2026 Evarentha
;; SPDX-License-Identifier: GPL-3.0-or-later

;; flattencom decoder ABI v1. No imports or WASI. Retain guest state between calls.
;; Replace the static response with protocol-specific JSON generation.
(module
  (memory (export "memory") 1)
  (data (i32.const 0) "{\22decoder\22:\22example\22,\22text\22:\22WASM decoded\22}")
  (func (export "alloc") (param i32) (result i32) i32.const 4096)
  ;; high 32 bits = output pointer 0; low 32 bits = JSON byte length 43
  (func (export "decode") (param i32 i32) (result i64) i64.const 43)
)
