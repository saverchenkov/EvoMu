# EvoMu Parity Acceptance & Attestation Guide

This document defines the parity tagging, runtime attestation, and triage conventions used across EvoMu to close rows in the canonical parity ledger (`.agents/skills/evomu-adversarial-review/references/parity/ledger.json`).

---

## 1. Parity Tag Convention (`// parity: <row id>`)

Every test that exercises and verifies an active feature or ledger row must be tagged with:

```rust
// parity: <category>:<identifier>[, <category>:<identifier>...]
```

### Tag Syntax Rules
1. **Placement**: The tag comment must appear either:
   - In the contiguous attribute/doc comment block immediately preceding the test function definition (`fn`), or
   - Within the first 12 lines of the test function body.
2. **Wildcards**: Wildcards (e.g. `*`) are **strictly prohibited** and rejected by the validator. Each row ID must be explicit.
3. **Multi-tagging**: Multiple row IDs may be comma-separated on a single line, or placed across multiple `// parity:` lines.
4. **Passing test required**: A row is only counted as `DONE` when the tagged test compiles, executes, and passes (`test <fn_name> ... ok`).
5. **Static implementation checks**:
   - `rpc:<service>.<method>`: Both `"<service>"` and `"<method>"` string literals must appear in the same non-test `.rs` file under `crates/`, and at least one occurrence must not be followed by `UnimplementedRPC`.
   - `notify:<event>`: The event string literal must appear in non-test server code.
   - `destiny:<action>`: The action identifier must appear in non-test code under `crates/`.
   - `esi:<route>`: The normalized route path must appear in server router code.
   - `plan:<id>`: All mapped `feat:` rows must be `DONE` or `NA`.

---

## 2. Runtime Attestation (`eve_testkit::attest`)

For large, data-driven test suites where individual tests verify hundreds or thousands of static data files or effects (`sde:*` and `effect:*`), row IDs can be attested at runtime via the `eve-testkit` crate.

### Usage in Data-Driven Tests
```rust
// In Cargo.toml:
// [dev-dependencies]
// eve-testkit = { path = "../eve-testkit" }

#[test]
fn test_sde_sweep() {
    // Assert real records against client data...
    
    // Attest covered ledger rows:
    eve_testkit::attest(&["sde:res:/staticdata/types.fsdbinary"]);
}
```

### Attestation Behavior
- When `$EVOMU_PARITY_ATTEST_DIR` is set to an existing directory (e.g. during `preflight.py` or `run_review.py`), `attest` appends a JSON line:
  ```json
  {"test": "<test_fn_name>", "rows": ["sde:res:/staticdata/types.fsdbinary"]}
  ```
  to `$EVOMU_PARITY_ATTEST_DIR/<crate_name>.jsonl`.
- When `$EVOMU_PARITY_ATTEST_DIR` is unset, empty, or not a directory, `attest` is a fast no-op.
- A runtime-attested row is only counted as `DONE` when the calling test passes in the test run.

---

## 3. Parity Triage for Non-Applicable Rows (`triage.json`)

Certain ledger rows represent client-internal logic, dead client code, or features removed on the live server. These rows can be triaged in `docs/parity/triage.json`.

### Triage Entry Schema
```json
{
  "<row_id>": {
    "disposition": "client_local | dead_client_code | removed_from_live",
    "evidence": "<path:line with at least 12 characters of evidence>"
  }
}
```

### Criteria
- The row must have `"na_allowed": true` in `ledger.json`.
- The disposition must be one of:
  - `"client_local"`: Executed solely within the client UI/runtime without server interaction.
  - `"dead_client_code"`: Unreachable or unreferenced legacy client code.
  - `"removed_from_live"`: Feature decommissioned or removed on the live server.
- The evidence must cite concrete client bytecode/source files (`path:line`) and contain at least 12 characters.
- Entries rejected by the adversarial gap auditor (`rejected_triage` in `auditor_rows.json`) are automatically invalidated.
