# [System / Feature Name]

## Overview & Lore Context
- High-level player concept, lore background, and role in New Eden.

## Canonical Mechanics & Formulas
- Complete rule set, numerical values, timers, formulas, and client UI interactions.
- Mathematical equations rendered in KaTeX.

## EvoMu Implementation Architecture
- **Crates Involved:** Direct links to involved crates (e.g. [`eve-server`](file:///Users/sergeya/ai/EvoMu/crates/eve-server), [`eve-destiny`](file:///Users/sergeya/ai/EvoMu/crates/eve-destiny)).
- **Primary Handlers & Dispatch:** MachoNet services, methods, Quasar handlers, ESI routes with direct source links.
- **State Store & Snapshot Schema:** Database structs and `WorldStateSnapshot` fields.
- **Tick & Lifecycle Integration:** 1 Hz Destiny tick, cluster maintenance tick.

## Invariants, Security & Edge Cases
- Session isolation, item locks, permission validation, exploit mitigation.
- Boundary conditions (warp, cloak, session change, system transitions).

## Verification, Tests & Parity References
- Pinned ledger rows (`feat:*`, `rpc:*`, `notify:*`).
- Concrete integration tests ([`crates/eve-server/tests/it/...`](file:///Users/sergeya/ai/EvoMu/crates/eve-server/tests/it/)).
