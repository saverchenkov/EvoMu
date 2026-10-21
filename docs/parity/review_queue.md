# Review queue and abandoned branches

## Abandoned (not merged; kept as archive tags for evidence)

- `archive/batch-g-hollow-727622f` (Batch G: notifications + RPC closure part 1). Dropped 2026-10-21 after a hardening audit kept 0 rows: notification emitters had no production callers (tests called builders directly), RPC arms were constant returns padded with dummy character lookups to evade the constant-handler lint, test-ID special cases in production code, shape-only and tautological tests, 17-36 tagged rows per test.
- `archive/b2-hollow-9f73e4c` (Batch B2 RPC closure + regression suites). Dropped 2026-10-21: built on the same constant/echo `batch_b_services` module and porting source as Batch G.
