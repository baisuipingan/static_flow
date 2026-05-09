# Kiro Attachment Protocol Parity Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Align StaticFlow's Kiro image/document handling with official Kiro IDE attachment semantics while preserving the existing public request shape.

**Architecture:** Keep the public Anthropic-style input contract stable, but replace the internal Kiro attachment pipeline with a first-class image/document model. Remove PDF-to-text rewriting, add document support to the Kiro wire layer, build official-style `images/documents` upstream payloads, and separate protocol limits from local service-protection limits.

**Tech Stack:** Rust, `serde_json`, existing `llm-access` / `llm-access-kiro` crates, targeted `cargo test`, `cargo clippy`, `rustfmt`.

---

## File Map

- `llm-access-kiro/src/wire.rs`
  - Add Kiro document wire types and document-bearing message fields
- `llm-access-kiro/src/anthropic/converter.rs`
  - Stop rewriting PDF/text documents into prompt text
  - Emit normalized text, image attachments, and document attachments separately
  - Add converter regression tests
- `llm-access/src/provider.rs`
  - Build official-style upstream `images/documents`
  - Enforce official attachment count and dedupe rules
  - Expand remote URL document normalization
  - Re-scope total-body guard as service protection

---

### Task 1: Lock in the current wrong behavior with failing tests

**Files:**
- Modify: `llm-access-kiro/src/anthropic/converter.rs`
- Modify: `llm-access/src/provider.rs`

- [ ] Add a failing converter test proving PDF document blocks must remain document attachments instead of becoming `PDF extracted text`.
- [ ] Add a failing converter test for a non-PDF supported document, preferably `text/markdown` or `text/html`, to prove text documents are also attachments, not prompt rewrites.
- [ ] Add a failing provider test proving more than 5 document attachments are rejected locally.
- [ ] Add a failing provider test proving more than 10 images are rejected locally.
- [ ] Add a failing provider test proving duplicate document names across conversation history are removed or rejected by the new deterministic rule.
- [ ] Run:

```bash
export CARGO_TARGET_DIR=/mnt/wsl/data4tb/static-flow-data/cargo-target/static_flow
cargo test -p llm-access-kiro --jobs 4
cargo test -p llm-access --jobs 4
```

- [ ] Confirm the new tests fail for the intended reason, not for unrelated compile errors.

### Task 2: Add document support to the Kiro wire model

**Files:**
- Modify: `llm-access-kiro/src/wire.rs`

- [ ] Add `KiroDocument` and `KiroDocumentSource` types matching the existing image wire style.
- [ ] Extend:
  - `UserInputMessage`
  - `UserMessage`
  - any current-message/history builders that currently only carry `images`
  so they can also carry `documents`.
- [ ] Keep the existing `images` field shape unchanged.
- [ ] Ensure serialization remains camelCase and empty document lists are skipped the same way images are skipped.
- [ ] Add focused wire tests if the file already contains serialization coverage; otherwise rely on downstream converter/provider tests.

### Task 3: Remove PDF text extraction from the converter

**Files:**
- Modify: `llm-access-kiro/src/anthropic/converter.rs`

- [ ] Replace `normalize_user_document_block(...)` so it validates and normalizes document blocks without rewriting them into `text`.
- [ ] Delete the `lopdf`-based extraction path:
  - `extract_pdf_document_text(...)`
  - xref repair helpers
  - `PDF_DOCUMENT_TEXT_PREFIX`
- [ ] Preserve strict source validation:
  - required `source`
  - required `source.type`
  - required `source.media_type`
  - required `source.data`
- [ ] Expand accepted document media types to the official supported set:
  - `application/pdf`
  - `text/csv`
  - `application/msword`
  - `application/vnd.openxmlformats-officedocument.wordprocessingml.document`
  - `application/vnd.ms-excel`
  - `application/vnd.openxmlformats-officedocument.spreadsheetml.sheet`
  - `text/html`
  - `text/plain`
  - `text/markdown`
- [ ] Keep text-only prompt normalization separate from attachment normalization.

### Task 4: Introduce a first-class internal attachment result from the converter

**Files:**
- Modify: `llm-access-kiro/src/anthropic/converter.rs`
- Modify: `llm-access-kiro/src/wire.rs`

- [ ] Refactor the converter output so the Kiro-facing build stage receives three independent channels:
  - normalized textual content
  - image attachments
  - document attachments
- [ ] Stop forcing downstream code to rediscover attachment meaning from already-rewritten `serde_json::Value`.
- [ ] Ensure both current-turn content and history content can emit attachments.
- [ ] Keep the existing public request parse boundary stable.

### Task 5: Rebuild upstream message assembly in `provider.rs`

**Files:**
- Modify: `llm-access/src/provider.rs`

- [ ] Update the Kiro upstream message builder so current-turn user input populates:
  - `content`
  - `images`
  - `documents`
- [ ] Update historical user messages so image/document attachments survive into `history`.
- [ ] Encode image formats only from the confirmed runtime-supported set:
  - JPEG
  - PNG
  - GIF
  - WEBP
- [ ] Encode document formats from the official supported set.
- [ ] Keep origin handling coherent with the existing image-origin rule; if document-bearing messages need the same treatment, make that decision explicit and test it.

### Task 6: Align attachment limits and dedupe rules with official Kiro behavior

**Files:**
- Modify: `llm-access/src/provider.rs`

- [ ] Add deterministic image limit enforcement: max 10 images per message flow.
- [ ] Add deterministic document limit enforcement: max 5 documents per conversation flow.
- [ ] Add message-local document-name dedupe.
- [ ] Add conversation-wide document-name dedupe across current message and history.
- [ ] Use stable, explainable local errors for these rejections.
- [ ] Do not reintroduce heuristic recovery branches.

### Task 7: Expand remote URL document normalization into the same attachment pipeline

**Files:**
- Modify: `llm-access/src/provider.rs`

- [ ] Extend remote URL document MIME normalization from the current `pdf/txt` subset to the official supported set.
- [ ] Keep remote image normalization limited to `jpeg/png/gif/webp`.
- [ ] Ensure remote URL fetched documents end up as the same internal document attachment type used by local/base64 documents.
- [ ] Keep SSRF and private-address protection unchanged.
- [ ] Add focused tests for:
  - remote PDF
  - remote markdown or html
  - remote docx/xlsx MIME acceptance

### Task 8: Demote the total-body cap into explicit service protection

**Files:**
- Modify: `llm-access/src/provider.rs`

- [ ] Revisit `KIRO_GENERATE_REQUEST_MAX_BODY_BYTES`.
- [ ] If kept, rename or document it as local service protection rather than upstream protocol semantics.
- [ ] Ensure local rejection messages distinguish:
  - official protocol boundary failures
  - local protection guard failures
- [ ] Preserve upstream `CONTENT_LENGTH_EXCEEDS_THRESHOLD` mapping unchanged.

### Task 9: Clean up obsolete dependencies and dead code

**Files:**
- Modify: `llm-access-kiro/src/anthropic/converter.rs`
- Modify: `llm-access-kiro/Cargo.toml` if `lopdf` becomes unused

- [ ] Remove dead helper code left by PDF text extraction removal.
- [ ] Remove no-longer-needed imports and constants.
- [ ] If `lopdf` is now unused, remove it from the crate dependency graph.
- [ ] Re-run search to confirm no stale `PDF extracted text` behavior remains:

```bash
rg -n "PDF extracted text|lopdf|extract_pdf_document_text|pdf_document_converted_to_text" llm-access-kiro llm-access
```

### Task 10: Verify locally and with live upstream probes

**Files:**
- Verify only

- [ ] Format only changed files:

```bash
rustfmt llm-access-kiro/src/wire.rs llm-access-kiro/src/anthropic/converter.rs llm-access/src/provider.rs
```

- [ ] Run crate tests:

```bash
export CARGO_TARGET_DIR=/mnt/wsl/data4tb/static-flow-data/cargo-target/static_flow
cargo test -p llm-access-kiro -p llm-access --jobs 4
```

- [ ] Run clippy with warnings at zero:

```bash
export CARGO_TARGET_DIR=/mnt/wsl/data4tb/static-flow-data/cargo-target/static_flow
cargo clippy -p llm-access-kiro -p llm-access --jobs 4 -- -D warnings
```

- [ ] Run live probes through the required proxy with a real Kiro auth:
  - image request
  - PDF request
  - at least one non-PDF document request
  - one request near the local size boundary

- [ ] Confirm the emitted upstream request shape contains `images/documents` instead of injected PDF text.

### Task 11: Final review before commit or release

**Files:**
- Review only

- [ ] Verify that public request compatibility is preserved.
- [ ] Verify there is no remaining dual-path PDF behavior.
- [ ] Verify local-protection limits are clearly separated from protocol limits.
- [ ] Verify tests cover:
  - current turn
  - history
  - URL media
  - count limits
  - duplicate names
  - upstream threshold mapping
