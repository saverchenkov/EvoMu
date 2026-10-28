# EvoMu Systems & Technical Wiki

Welcome to the **EvoMu Technical Wiki** — the definitive documentation portal for both the **canonical gameplay mechanics of EVE Online (V24.01)** and the **clean-room Rust implementation architecture of EvoMu**.

---

## Quick Navigation

Jump directly to any game system domain:

| | | |
| :--- | :--- | :--- |
| 🌌 [Universe & Navigation](#1-universe-space--celestial-navigation) | ⚙️ [Destiny Simulation & Physics](#2-ballpark-simulation--destiny-physics-engine) | 🏛️ [Upwell Structures & Starbases](#3-upwell-structures-starbases--orbital-deployment) |
| 🛸 [Living World & NPC AI](#4-living-world-ecology--npc-ai-systems) | 📋 [Missions, Agents & Standings](#5-missions-agent-divisions--pve-encounters) | 🔍 [Exploration, Scanning & Hacking](#6-exploration-scanning-hacking--relics) |
| ⛏️ [Mining & Harvesting](#7-mining-harvesting--resource-processing) | 🛠️ [Dogma Engine & Fitting](#8-dogma-attribute-engine-fitting--module-cycles) | ⚔️ [Combat Resolution & Gunnery](#9-combat-resolution-damage--crimewatch) |
| 🏭 [Industry & Manufacturing](#10-industry-manufacturing-science--reactions) | 🪐 [Planetary Industry (PI)](#11-planetary-industry-pi) | 💰 [Economy, Markets & Contracts](#12-player-economy-markets--contracts) |
| 🏢 [Corporations & Alliances](#13-corporations-alliances--social-governance) | 🚩 [Sovereignty & Faction Warfare](#14-sovereignty-faction-warfare--territorial-conflict) | 🧬 [Character Progression & Clones](#15-character-progression-skills--medical-clones) |
| 🔌 [Protocol & Infrastructure](#16-protocol-architecture-machonet-quasar--sde) | 📝 [Article Template](./templates/article_template.md) | 🏠 [Repository Root](../../README.md) |

---

## Core Architecture & Engine Crates

EvoMu is engineered in modern Rust (2021 edition) with `#![forbid(unsafe_code)]` across all gameplay code, enforcing strict memory safety and deterministic 1 Hz simulation:

* [**`eve-proto`**](../../crates/eve-proto): Binary marshal/unmarshal codec, PyRep wire representations, and type descriptors.
* [**`eve-machonet`**](../../crates/eve-machonet): MachoNet packet protocol, session multiplexing, and cryptographic session cookies.
* [**`eve-destiny`**](../../crates/eve-destiny): Deterministic 1 Hz physics simulation loop, spatial ballparks, and collision kinematics.
* [**`eve-dogma`**](../../crates/eve-dogma): Dynamic attribute modifier graphs, operator stacking, and combat/fitting calculators.
* [**`eve-db`**](../../crates/eve-db): Transactional world state snapshots (`WorldStateSnapshot`), item tables, and SQLite persistence.
* [**`eve-sde`**](../../crates/eve-sde): Static Data Export (SDE) loaders, solar system topologies, and item type definitions.
* [**`eve-server`**](../../crates/eve-server): Master gameplay server, domain service handlers, living world director, and ESI OpenAPI routes.

---

## Master Articles Directory

| Domain | Article | Key Canonical Mechanics | Core Rust Implementations |
| :--- | :--- | :--- | :--- |
| **Universe & Nav** | [**Pochven Navigation & Anchoring**](./universe_and_navigation/pochven_navigation_and_anchoring.md) | Clades, conduits, standing locks (+6.0 Home), filament pools, 30 km wormhole offset, citadel ban | [`space_nav_travel.rs`](../../crates/eve-server/src/services/space_nav_travel.rs), [`station_structure.rs`](../../crates/eve-server/src/services/station_structure.rs) |
| **Universe & Nav** | [**Stargate Jumps & Cynosural Nav**](./universe_and_navigation/stargate_jumping_and_cynosural_navigation.md) | Stargate transitions, 10s session lock, 60s gate cloak, cynos, Titan bridging, jump fatigue | [`space_warp_jump.rs`](../../crates/eve-server/src/services/space_warp_jump.rs), [`fitting_cyno_portal.rs`](../../crates/eve-server/src/services/fitting_cyno_portal.rs) |
| **Ballpark & Physics** | [**Destiny Simulation Loop**](./ballpark_and_physics/destiny_simulation_loop.md) | Deterministic 1 Hz tick, sub-warp velocity integration ($v(t)$), align time, 13 `DSTBALL` modes, warp pipeline | [`ballpark.rs`](../../crates/eve-destiny/src/ballpark.rs), [`physics.rs`](../../crates/eve-destiny/src/physics.rs) |
| **Upwell Structures** | [**Upwell Structures Overview**](./upwell_and_structures/upwell_structures_overview.md) | Citadels/complexes, 1,000 km rule, 15m/30m reinforce timers, Quantum Cores, ACLs, asset safety | [`structure_lifecycle.rs`](../../crates/eve-server/src/services/structure_lifecycle.rs), [`structure_acl_defense_rules.rs`](../../crates/eve-server/src/services/structure_acl_defense_rules.rs) |
| **Living World & NPCs** | [**NPC Combat AI & Ecology**](./npcs_and_living_world/npc_combat_ai_and_ecology.md) | 3-tier LOD simulation, 1 Hz combat AI state machine, belt rats, CONCORD response, hauler convoys | [`combat_npcs_drones.rs`](../../crates/eve-server/src/services/combat_npcs_drones.rs), [`combat_npc_police.rs`](../../crates/eve-server/src/services/combat_npc_police.rs) |
| **Missions & Agents** | [**Agent Dialogue, Missions & LP**](./missions_and_agents/agent_dialogue_and_missions.md) | Security/Mining/Distribution agents, mission state machine, decline penalty, 16-mission storylines, LP stores | [`agents_missions.rs`](../../crates/eve-server/src/services/agents_missions.rs), [`agents_missions_helpers.rs`](../../crates/eve-server/src/services/agents_missions_helpers.rs) |
| **Exploration** | [**Scanning, D-Scan & Hacking**](./exploration_and_hacking/hacking_minigame_and_sites.md) | D-Scan cone scanning, 8-probe formation deviation reduction, 24-hex node hacking grid, virus coherence | [`pve_exploration.rs`](../../crates/eve-server/src/services/pve_exploration.rs), [`pve_hacking_sites.rs`](../../crates/eve-server/src/services/pve_hacking_sites.rs) |
| **Mining** | [**Mining Cycles & Residue**](./mining_and_harvesting/mining_cycles_and_residue.md) | Laser cycle engine, yield formulas, crystal volatility, residue math, ice/gas, Metenox moon drills | [`mining_cycle.rs`](../../crates/eve-server/src/services/mining_cycle.rs), [`mining_advanced.rs`](../../crates/eve-server/src/services/mining_advanced.rs) |
| **Dogma & Fitting** | [**Dogma Engine & Modifier Graphs**](./dogma_and_fitting/dogma_attribute_engine.md) | Modifier graph with 9 stages, non-linear stacking penalty decay $S(i)$, Nosferatu relative %, heat propagation | [`evaluation.rs`](../../crates/eve-dogma/src/evaluation.rs), [`fitting_module_cycles.rs`](../../crates/eve-server/src/services/fitting_module_cycles.rs) |
| **Combat & Crimewatch** | [**Crimewatch & Timers**](./combat_and_crimewatch/crimewatch_and_engagement_timers.md) | Weapons (60s), PvP (15m), Suspect/Criminal (15m), safety interlocks, CONCORD response, safe logoff | [`combat_crimewatch.rs`](../../crates/eve-server/src/services/combat_crimewatch.rs), [`crimewatch_timers.rs`](../../crates/eve-server/src/services/crimewatch_timers.rs) |
| **Combat & Crimewatch** | [**Gunnery & Missile Kinematics**](./combat_and_crimewatch/gunnery_tracking_and_missile_kinematics.md) | Turret tracking equation, angular velocity, quality bands & wrecking shots, missile DRF, death pipeline | [`combat_weapons.rs`](../../crates/eve-server/src/services/combat_weapons.rs), [`combat_missiles_aoe.rs`](../../crates/eve-server/src/services/combat_missiles_aoe.rs) |
| **Industry** | [**Blueprints & Manufacturing**](./industry_and_manufacturing/blueprints_manufacturing_and_reactions.md) | BPO/BPC, ME/TE math, facility rig scaling, System Cost Index (SCI), T2 invention, composite reactions | [`industry_facilities_bps.rs`](../../crates/eve-server/src/services/industry_facilities_bps.rs), [`industry_jobs.rs`](../../crates/eve-server/src/services/industry_jobs.rs) |
| **Planetary Industry** | [**Planetary Industry (PI) & POCOs**](./planetary_industry/colony_pin_networks_and_pi.md) | 8 planet types, spherical harmonics, Command Centers, ECUs, P1–P4 production, POCO import/export tax ACLs | [`pi_colonies.rs`](../../crates/eve-server/src/services/pi_colonies.rs), [`pi_orbitals.rs`](../../crates/eve-server/src/services/pi_orbitals.rs) |
| **Economy & Markets** | [**Market Orders & Contracts**](./economy_and_markets/regional_market_order_matching.md) | CLOB double auction, Maker/Taker precedence, escrow locks, broker fees, OHLCV history, courier contracts | [`market_trading.rs`](../../crates/eve-server/src/services/market_trading.rs), [`contracts_service.rs`](../../crates/eve-server/src/services/contracts_service.rs) |
| **Corporations** | [**Corporation Roles & Governance**](./corporations_and_governance/corporation_roles_and_governance.md) | Corp creation, 24-hour role grant delay invariant, 7 hangars/wallets, shareholder voting, projects | [`fleet_social_corps.rs`](../../crates/eve-server/src/services/fleet_social_corps.rs), [`fleet_social_corp_gov.rs`](../../crates/eve-server/src/services/fleet_social_corp_gov.rs) |
| **Corporations** | [**Alliances & Formal Wars**](./corporations_and_governance/alliances_and_formal_wars.md) | Alliance charters, 1B ISK fee, War HQ physical structure anchor, 24h war spool-up, surrender treaties | [`social_wars.rs`](../../crates/eve-server/src/services/social_wars.rs), [`social_corp_projects.rs`](../../crates/eve-server/src/services/social_corp_projects.rs) |
| **Sovereignty & FW** | [**Faction Warfare & Insurgencies**](./sovereignty_and_faction_warfare/faction_warfare_and_insurgencies.md) | Frontlines graph topology, Advantage multipliers, Havoc Pirate FOBs, Corruption/Suppression 1-5, ESS | [`fw_services.rs`](../../crates/eve-server/src/services/fw_services.rs), [`insurgency_services.rs`](../../crates/eve-server/src/services/insurgency_services.rs) |
| **Sovereignty & FW** | [**Sov Hubs & Equinox Skyhooks**](./sovereignty_and_faction_warfare/sovereignty_hubs_and_equinox_skyhooks.md) | Unified Equinox Sov Hub (Type 32458), Entosis ADM scaling (1.0x-6.0x), Power/Workforce, Skyhooks | [`sov_services.rs`](../../crates/eve-server/src/services/sov_services.rs), [`quasar_sov_skyhook.rs`](../../crates/eve-server/src/services/quasar_sov_skyhook.rs) |
| **Character & Clones** | [**Skill Training & Medical Clones**](./character_and_progression/skill_training_and_medical_clones.md) | Training speed ($SP/\text{min}$), rank formula, Alpha/Omega tiers, clone respawn, jump clone 24h timer | [`character_skill_ops.rs`](../../crates/eve-server/src/services/character_skill_ops.rs), [`character_clones_implants.rs`](../../crates/eve-server/src/services/character_clones_implants.rs) |
| **Protocol & Infra** | [**MachoNet, Quasar & Wire**](./protocol_and_infrastructure/machonet_wire_and_quasar_gateway.md) | 4-byte LE framing, GPS handshake, PyRep inverted dict codec, DBRow descriptors, Quasar protobuf, ESI | [`handshake.rs`](../../crates/eve-machonet/src/handshake.rs), [`dispatch.rs`](../../crates/eve-server/src/router/dispatch.rs) |

---

## Detailed System Guides

### 1. Universe, Space & Celestial Navigation

The celestial topology defines New Eden across K-space (High-Sec, Low-Sec, Null-Sec), J-space (Wormhole systems), the isolated Pochven triangular pocket, and ancient Zarzakh:

* [**Pochven Navigation, Filaments & Anchoring Restrictions**](./universe_and_navigation/pochven_navigation_and_anchoring.md):
  Comprehensive analysis of the 27 conquered systems, Cladistic triangular conduits, standing restrictions (+6.0 to enter Home systems Kraask, Dazh, and Veles), Pochven wormhole 30 km spatial offsets, and the total ban on Upwell structure anchoring.
* [**Stargate Jumps, Cynosural Navigation & Jump Fatigue**](./universe_and_navigation/stargate_jumping_and_cynosural_navigation.md):
  Deterministic mechanics of gate transitions, 10-second session change locks, 60-second invulnerability cloaks, cynosural beacon classifications (Standard, Covert, Industrial), capital jump drive fuel consumption, Titan/Black Ops bridging pipelines, and Phoebe jump fatigue decay.

### 2. Ballpark Simulation & Destiny Physics Engine

The spatial simulation layer runs inside `eve-destiny` at a deterministic 1 Hz world tick:

* [**Destiny Physics Simulation Loop & Kinematics**](./ballpark_and_physics/destiny_simulation_loop.md):
  Sub-warp velocity vector integration, maximum speed asymptotes, align time formulas ($t_{\text{align}} = -\ln(0.25) \cdot \frac{M \cdot A}{10^6}$), the 13 canonical `DSTBALL_*` motion modes, the 5-phase warp state machine (Align $\to$ Accelerate $\to$ Coast $\to$ Decelerate $\to$ Exit), and warp bubble displacement geometry.

### 3. Upwell Structures, Starbases & Orbital Deployment

Player-owned orbital infrastructure operating under modern Upwell Consortium architecture:

* [**Upwell Structures Overview & Lifecycle**](./upwell_and_structures/upwell_structures_overview.md):
  Upwell taxonomy (Astrahus, Fortizar, Keepstar, Raitaru, Azbel, Sotiyo, Athanor, Tatara), the 1,000 km exclusion radius rule, 4-phase deployment, 15m/30m reinforcement timers, damage caps, Quantum Core Deed Bay insertion, Access Control Lists (ACLs), and the 20-day Asset Safety recovery pipeline.

### 4. Living World, Ecology & NPC AI Systems

Autonomous non-capsuleer entities populating the New Eden solar systems:

* [**NPC Combat AI & Living World Ecology**](./npcs_and_living_world/npc_combat_ai_and_ecology.md):
  3-tier Level of Detail (LOD) background simulation, 1 Hz combat AI state machine (Idle $\to$ Approach $\to$ Orbit $\to$ Engage), dynamic target switching based on threat matrices, pirate faction belt rats, CONCORD emergency response pipelines, faction navy border police, and autonomous hauler supply convoys.

### 5. Missions, Agent Divisions & PvE Encounters

Structured capsuleer PvE contracts, agent dialogue, and faction standings:

* [**Agent Dialogue, Missions, Standings & LP Stores**](./missions_and_agents/agent_dialogue_and_missions.md):
  Security, Mining, and Distribution mission lifecycles; decline cooldown penalties (4 hours); 16-mission storyline tally triggering faction awards; Loyalty Point (LP) store offer matching; and fractional standing decay/gain equations.

### 6. Exploration, Scanning, Hacking & Relics

Sensory exploration of New Eden cosmic signatures and archaeological sites:

* [**Exploration Scanning, Directional Scanner (D-Scan) & Hacking Minigame**](./exploration_and_hacking/hacking_minigame_and_sites.md):
  Directional Scanner cone mechanics (5° to 360°), 8-probe formation scan deviation reduction, 24-hex axial node hacking minigame, virus coherence and strength math, defensive subsystems (Firewalls, Antivirus, Kernels), and Ghost Site detonation timers.

### 7. Mining, Harvesting & Resource Processing

Extraction and refining of raw minerals, ice blocks, gas clouds, and moon ores:

* [**Mining Cycles, Residue & Resource Extraction Systems**](./mining_and_harvesting/mining_cycles_and_residue.md):
  Deterministic harvesting laser cycles, yield formulas, crystal volatility rolls, residue waste probabilities (Type A/B/C), specialized ore/mineral cargo holds, automated Metenox moon drills, and in-space industrial compression (Porpoise, Orca, Rorqual).

### 8. Dogma Attribute Engine, Fitting & Module Cycles

The mathematical attribute evaluation and dynamic modifier engine powering all combat, ships, and modules:

* [**Dogma Attribute Engine, Modifier Graphs & Fitting**](./dogma_and_fitting/dogma_attribute_engine.md):
  9-stage modifier graph dependency resolution, non-linear stacking penalty decay function $S(i) = e^{-(i/2.67)^2}$, differential capacitor recharge $\frac{dC}{dt}$, Nosferatu relative capacitor caps, module activation cycles, and rack overheating thermodynamics.

### 9. Combat Resolution, Damage & Crimewatch

Fleet engagements, turret kinematics, missile flight, and legal consequences:

* [**Crimewatch, Aggression Rules & Engagement Timers**](./combat_and_crimewatch/crimewatch_and_engagement_timers.md):
  Weapons timer (60s), PvP combat timer (15m), Suspect and Criminal flags (15m), safety interlocks (Green, Yellow, Red), CONCORD rapid-intervention death pipeline, and legal aggression rules across security zones.
* [**Gunnery Tracking, Missile Kinematics & Damage Application**](./combat_and_crimewatch/gunnery_tracking_and_missile_kinematics.md):
  Turret hit chance equation with angular velocity and signature resolution, quality damage bands (Scratch to Wrecking 3x), missile Damage Reduction Factor (DRF) kinematic formulas, shield passive recharge curves, and authentic killmail generation.

### 10. Industry, Manufacturing, Science & Reactions

Capsuleer production of ships, ammunition, modules, and structures:

* [**Blueprints, Manufacturing, Science & Reactions**](./industry_and_manufacturing/blueprints_manufacturing_and_reactions.md):
  Original vs. Copy blueprints (BPO/BPC), Material and Time Efficiency (ME/TE) mathematical formulas, dynamic System Cost Index (SCI) based on regional activity, Tech II invention probabilities, and refinery composite/biochemical reactions.

### 11. Planetary Industry (PI)

Extraterrestrial resource exploitation across planetary surfaces:

* [**Planetary Industry (PI), Colony Pin Networks & Customs Offices**](./planetary_industry/colony_pin_networks_and_pi.md):
  The 8 planetary archetypes, spherical harmonic resource concentration heatmaps, Command Center upgrade tiers, Extractor Control Units (ECUs), P1 through P4 production chains, routing link bandwidth, and Player-Owned Customs Office (POCO) tax ACLs.

### 12. Player Economy, Markets & Contracts

Decentralized market liquidity, order matching, and formal legal contracts:

* [**Regional Market Engine, Order Matching & Contracts**](./economy_and_markets/regional_market_order_matching.md):
  Continuous Limit Order Book (CLOB) double auction matching, Maker vs. Taker price execution, immediate escrow locking, broker fees, transaction accounting tax, OHLCV market history snapshots, and item exchange / courier contracts.

### 13. Corporations, Alliances & Social Governance

Social institutions, legal warfare, and hierarchical administration:

* [**Corporation Roles, Governance & Asset Management**](./corporations_and_governance/corporation_roles_and_governance.md):
  Corporation creation, 24-hour role grant delay security invariant, 7 independent corporate hangars and wallets, share issuance, shareholder voting cases, and corporate project tracking.
* [**Alliances, War Declarations & War HQ Mechanics**](./corporations_and_governance/alliances_and_formal_wars.md):
  Alliance registration charters (1B ISK fee), formal war declarations, physical War HQ structure anchoring in high-sec, 24-hour war spool-up timers, mutual peace treaties, and allied defender pacts.

### 14. Sovereignty, Faction Warfare & Territorial Conflict

Large-scale geopolitical warfare across low-sec warzones and null-sec empires:

* [**Faction Warfare, Pirate Insurgencies & Encounter Surveillance System (ESS)**](./sovereignty_and_faction_warfare/faction_warfare_and_insurgencies.md):
  Frontline, Command, and Rearguard graph topology; Advantage capture point scaling; Havoc Pirate Forward Operating Bases (FOBs); Corruption and Suppression stages 1-5; Dynamic Bounty System (DBS); and ESS Main/Reserve Bank hacking.
* [**Sovereignty Hubs, Equinox Orbital Skyhooks & Entosis Mechanics**](./sovereignty_and_faction_warfare/sovereignty_hubs_and_equinox_skyhooks.md):
  Unified Equinox Sovereignty Hub (Type 32458), Entosis capture timers scaled by Activity Defense Multiplier (1.0x to 6.0x), planetary Power/Workforce resource allocations, Orbital Skyhooks (Type 81080), and Siphon Bay raiding.

### 15. Character Progression, Skills & Medical Clones

Capsuleer identity, neurological attribute matrices, and resurrection:

* [**Character Progression, Skill Training & Medical Clones**](./character_and_progression/skill_training_and_medical_clones.md):
  Attribute-dependent skill point training rates ($SP/\text{min} = \text{Primary} + \frac{\text{Secondary}}{2}$), exponential skill rank equations, Alpha versus Omega clone distinctions, medical clone death respawn, jump clone 24-hour cooldowns, and cybernetic attribute implants.

### 16. Protocol Architecture, MachoNet, Quasar & SDE

Network wire encoding, remote procedure calls, and static game data:

* [**MachoNet Protocol, Quasar Services & Wire Infrastructure**](./protocol_and_infrastructure/machonet_wire_and_quasar_gateway.md):
  4-byte Little-Endian packet framing, GPS wire handshake state machine, CRC-CCITT hash checks, PyRep marshal codec with inverted dict compression, CRowset/DBRow binary table descriptors, Quasar protobuf gateway streaming, and ESI OpenAPI routing.

---

## Contributing & Standard Article Structure

All wiki deep-dives adhere to the canonical 5-part architecture defined in the [**Wiki Article Template**](./templates/article_template.md):

1. **Overview & Lore Context**: Architectural placement, New Eden background, and high-level concept diagrams (`mermaid`).
2. **Canonical Game Mechanics & Mathematical Specifications**: Detailed formulas, KaTeX mathematics, state machines, and exact constants matching CCP V24.01 server behavior.
3. **EvoMu Implementation Architecture**: Clean-room Rust crate organization, data structures, RPC handlers, and 1 Hz world tick loops.
4. **Invariants, Edge Cases & Security Posture**: Safety invariants, permission models, anti-exploit checks, and atomicity guarantees.
5. **Concrete Test Coverage & Parity References**: Test suite files, verified parity ledger keys, and commands to run tests.
6. **Related Systems & Cross-References**: Bidirectional markdown links connecting related domains.
