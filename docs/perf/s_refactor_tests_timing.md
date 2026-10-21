# S-REFACTOR-TESTS timing evidence

These are raw measurements, copied verbatim from the tool output that produced them on 2026-10-20.

## Test loop (`EvoMu/.evomu-reviews/preflight_history.jsonl`, written by `preflight.py`)

```
{"ts": "2026-10-20T20:34:43", "worktree": "EvoMu-na-sde-destiny", "branch": "main", "head": "895f50a", "wall_s": 256.1, "build_s": 9.43, "run_sum_s": 191.9, "binaries": 68, "passed": 263, "failed": 0}
{"ts": "2026-10-20T20:38:07", "worktree": "EvoMu-s-refactor-tests", "branch": "wip/s-refactor-tests", "head": "849e4b9", "wall_s": 94.2, "build_s": 0.1, "run_sum_s": 80.3, "binaries": 31, "passed": 263, "failed": 0}
```

## Cold eve-server rebuild

Both runs used the same `CARGO_TARGET_DIR`. Each tree's dependencies were built first, then this was run:

```
cargo clean -p eve-server && cargo test --workspace --no-run
```

`main` (895f50a):
```
     Removed 16668 files, 5.4GiB total
    Finished `test` profile [unoptimized + debuginfo] target(s) in 22.65s
```

`wip/s-refactor-tests`:
```
     Removed 4503 files, 2.3GiB total
    Finished `test` profile [unoptimized + debuginfo] target(s) in 14.06s
```
