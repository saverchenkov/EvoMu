# [System / Feature Name]

> **Navigation**: [EvoMu Technical Wiki](../README.md) • [Related Systems](#related-systems--cross-references)
>
> [← Back to Wiki Table of Contents](../README.md)

---

## Overview & Lore Context
- High-level player concept, lore background, and role in New Eden.
- Concept diagrams (`mermaid`).

## Canonical Mechanics & Formulas
- Complete rule set, numerical values, timers, formulas, and client UI interactions.
- Mathematical equations rendered in KaTeX.

## EvoMu Implementation Architecture
- **Crates Involved:** Direct relative links to crates (e.g. [`eve-server`](../../../crates/eve-server), [`eve-destiny`](../../../crates/eve-destiny)).
- **Primary Handlers & Dispatch:** MachoNet services, methods, Quasar handlers, ESI routes with direct source links.
- **State Store & Snapshot Schema:** Database structs and `WorldStateSnapshot` fields.
- **Tick & Lifecycle Integration:** 1 Hz Destiny tick, cluster maintenance tick.

## Invariants, Security & Edge Cases
- Session isolation, item locks, permission validation, exploit mitigation.
- Boundary conditions (warp, cloak, session change, system transitions).

## Verification, Tests & Parity References
- Pinned ledger rows (`feat:*`, `rpc:*`, `notify:*`).
- Concrete integration tests ([`crates/eve-server/tests/it/`](../../../crates/eve-server/tests/it/)).

---

## Related Systems & Cross-References
- [**EvoMu Technical Wiki Table of Contents**](../README.md): Master index and domain directory.

---

[← Back to Wiki Table of Contents](../README.md)
