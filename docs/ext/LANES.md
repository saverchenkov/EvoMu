# External agent lanes

External agents (no messaging; see `AGENTS.md`, "External agents") work only inside their lane. The coder doesn't edit files in an active lane. The supervisor maintains this file.

| Lane | Status | Owner | Paths (exclusive while active) | Goal |
|---|---|---|---|---|
| `core-quality` | **cancelled 2026-10-21 16:38** (never started; paths released to the coder) | — | `crates/eve-proto/**`, `crates/eve-machonet/**`, `crates/eve-destiny/**`, `crates/eve-dogma/**`, `fuzz/**`, `.github/workflows/**` | Bring these crates to final-gate quality: line coverage ≥ 90% and cargo-mutants score ≥ 85% per crate, through meaningful tests and real bug fixes found by mutants and fuzzing. Add Linux CI. |

Rules for lane changes:
- Public API changes in a lane crate need a reason in the status file (`docs/ext/core-quality.md`), and must keep `cargo build --workspace` and the whole test suite green. `eve-server` call sites may only be touched mechanically (signature follow-ups), never behaviourally.
- Bug fixes come with a regression test that fails before the fix.
