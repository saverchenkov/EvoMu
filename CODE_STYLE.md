# EvoMu Rust Code Style & Engineering Conventions

This document establishes the code style, architectural conventions, and engineering standards for the **EvoMu** project. All developers, coding agents, and subagents must adhere to these standards.

---

## 1. Industry Baseline References

EvoMu adopts standard industry guidelines for language-level ergonomics and formatting:

1. **[The Rust API Guidelines (RFC 430)](https://rust-lang.github.io/api-guidelines/):**
   - **Naming:** `UpperCamelCase` for types and traits; `snake_case` for functions, methods, modules, and local variables; `SCREAMING_SNAKE_CASE` for `const` and `static`.
   - **Conversions:** Follow standard prefix conventions:
     - `as_`: Free, borrowed conversion (e.g., `as_str()`, `as_bytes()`).
     - `to_`: Expensive or clone/allocation-based conversion (e.g., `to_string()`, `to_wire_f64()`).
     - `into_`: Consuming conversion that moves ownership (e.g., `into_inner()`, `into_macho()`).
   - **Error Handling:** Propagate recoverable errors via `Result<T, E>`. Avoid panicking (`panic!`, `.unwrap()`, `.expect()`) in public library and server dispatch code.
2. **[Official Rust Style Guide (RFC 2436)](https://doc.rust-lang.org/nightly/style-guide/):**
   - Formatted via standard `cargo fmt` with 4-space indentation and 100-character line preference.

---

## 2. Commenting & Documentation Standards

As gameplay mechanics, edge cases, and protocol layers grow, **undocumented nested code branches become unmaintainable**. Code must be self-explanatory to both human engineers and AI agents.

### 2.1 Explain the "Why", Not the Syntax
Do not simply restate what the Rust syntax is doing. Explain the underlying **EVE Online game mechanic, client protocol requirement, or state invariant**.

```rust
// ❌ BAD: Simply restating the syntax
// Check if target is not parent and distance is less than aoe_radius
if b.id != parent_id && d <= aoe_radius { ... }

// ✅ GOOD: Explaining the game rule and mechanical intent
// In EVE mechanics, AoE bomb explosions damage all ships, drones, and structures within radius,
// excluding the launching fighter squad and its parent carrier.
if b.id != fighter_ball_id && b.id != fighter.parent_ship_id && d <= aoe_radius { ... }
```

### 2.2 Document Complex & Nested Branches
Any nested control flow, multi-hop snapshot query, or non-obvious fallback requires a 1–2 line comment explaining what entity is being resolved and why:

```rust
// ✅ GOOD: Documenting multi-step resolution
// Resolve victim character ID: check active player ships first, then fallback to
// item owner for deployed structures or secure containers.
let bomb_victim = ctx.db.with_snapshot(|snap| {
    snap.characters.values().find(|c| c.active_ship_id == b.id).map(|c| c.character_id)
        .or_else(|| snap.items.get(&b.id).map(|i| i.owner_id).filter(|oid| snap.characters.contains_key(oid)))
});
```

### 2.3 Traceability & Parity Tags
When code implements a specific parity requirement, fixes a known audit finding, or patches an exploit, tag it directly inline:

- `// Parity: feat:drones.fighter-support-abilities`
- `// Fixes OF-51: AoE weapon hits must apply victim PvP aggression timers to prevent combat logging`
- `// Fixes OF-47: Validate client float input to reject NaN/inf bounties`

---

## 3. Concurrency & Lock Order Invariants

To eliminate deadlocks (specifically ABBA lock inversions between the async system actors and the synchronous database store), follow this strict hierarchy:

1. **Global Lock Hierarchy:**
   ```
   Systems Registry (ctx.systems)
     └── Ballpark State (sys_handle.ballpark)
           └── Database Snapshot Store (ctx.db / WorldStateSnapshot)
   ```
2. **Never Hold DB Snapshot Locks Across External Calls:**
   - **Rule:** Never call into `ctx.systems`, actor message channels, or `.await` points while holding a `WorldStateSnapshot` write lock or `with_mut_sp6_state` lock guard.
   - **Remedy:** Scope locks using explicit code blocks `{ ... }`, or use a **read-clone-writeback pattern**:
     ```rust
     // Extract required data under read lock, drop the lock, perform external calls, then write back:
     let targets = ctx.db.with_snapshot(|snap| snap.get_pending_targets());
     let result = ctx.systems.is_ball_scrambled_sync(targets);
     ctx.db.try_with_mut_snapshot(|snap| { snap.apply_result(result); Ok(()) })?;
     ```

---

## 4. RPC & Protocol Safety

1. **Unhandled Beats Wrong (Action Plan Rule 0.4):**
   - If an RPC method or opcode is not yet backed by real persistent database state or SDE logic, return `UnimplementedRPC`.
   - **Never** ship mock echo handlers (returning caller's input), constant fake IDs, or synthetic seed fixtures in production dispatch paths.
2. **Never Panic on Client Inputs:**
   - Do not call `.unwrap()` or index directly (`args[0]`) on client-supplied `PyRep` or `DynamicValue` messages.
   - Always use `.get(...)`, `.and_then(...)`, or `?` with a clean `BridgeRpcError`:
     ```rust
     let target_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
     if target_id <= 0 {
         return Some(Err(BridgeRpcError::invalid_argument("Target ID required")));
     }
     ```
3. **Role & Authority Verification:**
   - Every mutation acting on a corporation, structure, fleet, or account must explicitly verify ownership and CCP role bits (`require_corp_role`, `require_admin_role`, `is_fleet_boss`). Never fail open.

---

## 5. Numeric Safety & Currency Types

1. **Fixed-Point Currency:**
   - **Never** store or calculate ISK or LP using raw `f64`.
   - Use `IskCents` (integer fixed-point representing 1/100th of an ISK). Convert to/from wire representations only at the outermost protocol boundary.
2. **Float Validation on Client Input:**
   - When accepting float parameters from the client (e.g. bounty amounts, distances, coordinates), always assert `.is_finite()`:
     ```rust
     if !amount_isk.is_finite() || amount_isk < 100_000.0 {
         return Some(Err(BridgeRpcError::invalid_argument("Invalid bounty amount")));
     }
     ```
3. **Checked Arithmetic:**
   - Use `checked_add`, `checked_sub`, or `saturating_*` operations when calculating player wallet balances, skill points, or inventory quantities to prevent overflow panics or minting exploits.

---

## 6. Persistence & Atomicity Invariants

1. **Single-Transaction Atomicity:**
   - If a player operation modifies both a wallet balance and an inventory/bounty/market pool, both mutations **must** happen inside the same `try_with_mut_snapshot` closure.
   - Never split a financial transfer into two independent database commits (e.g., debiting wallet in transaction A, then crediting pool in transaction B).
2. **No Unpersisted Side-Stores:**
   - Gameplay state must never live in isolated process-global statics (`OnceLock`, `Mutex`, `LazyLock`) that diverge from SQLite or vanish on server restart. All state must reside in `WorldStateSnapshot` with corresponding SQLite migrations.
