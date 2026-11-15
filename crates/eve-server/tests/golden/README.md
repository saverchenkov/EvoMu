# Destiny / Motion Harness Golden Sequences

This directory holds committed golden action/relative-stamp sequences (and optional captured session traces) for the harness journeys tracked in issue #1077 (`§B` and `§D`).

## Trace & Golden Format

- **Captured trace** (`traces/<scenario>.trace.json`, schema in [trace_schema.json](trace_schema.json)): records virtual clock steps (`clock_advance`), inbound harness RPCs (`rpc_in`), and outbound server frames (`out_frame`, including `DoDestinyUpdate` actions and timestamps).
- **Golden file** (`<scenario>.json`): deterministic per-session sequence of `{"rel_stamp", "action", "entities"}` entries produced by replaying the trace under `tools/replay/trace.py::VirtualClock`.

## Verifying & Updating Goldens

Verify a trace against its committed golden sequence and check all virtual-clock and destiny invariants:

```sh
python3 tools/replay/verify_trace.py --trace <path/to/trace.json> --golden crates/eve-server/tests/golden/<scenario>.json
```

Or by scenario name:

```sh
python3 tools/replay/verify_scenario.py --list
python3 tools/replay/verify_scenario.py <scenario> --trace <path/to/trace.json>
```

To regenerate a golden file after an intentional protocol/sequence change:

```sh
UPDATE_GOLDEN=1 python3 tools/replay/verify_trace.py --trace <path/to/trace.json> --golden crates/eve-server/tests/golden/<scenario>.json
```
