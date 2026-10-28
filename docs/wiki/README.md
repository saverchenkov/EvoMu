# EvoMu Systems & Technical Wiki

Welcome to the **EvoMu Technical Wiki** — the definitive documentation portal for both the **canonical gameplay mechanics of EVE Online (V24.01)** and the **clean-room Rust implementation architecture of EvoMu**.

---

## Architecture Overview & Philosophy

EvoMu is structured around high-concurrency, memory-safe Rust crates that simulate the entire EVE Online universe:
* [`eve-proto`](file:///Users/sergeya/ai/EvoMu/crates/eve-proto): Binary marshal/unmarshal codec and PyRep wire representations.
* [`eve-machonet`](file:///Users/sergeya/ai/EvoMu/crates/eve-machonet): MachoNet packet protocol, session state management, and call masking.
* [`eve-destiny`](file:///Users/sergeya/ai/EvoMu/crates/eve-destiny): Deterministic 1 Hz physics engine, spatial ballparks, and collision kinematics.
* [`eve-dogma`](file:///Users/sergeya/ai/EvoMu/crates/eve-dogma): Dynamic attribute modifier graphs, operator stacking, and combat calculations.
* [`eve-db`](file:///Users/sergeya/ai/EvoMu/crates/eve-db): Transactional world state snapshots (`WorldStateSnapshot`) and SQLite persistence.
* [`eve-sde`](file:///Users/sergeya/ai/EvoMu/crates/eve-sde): Static Data Export (SDE) loaders, celestial topologies, and item type definitions.
* [`eve-server`](file:///Users/sergeya/ai/EvoMu/crates/eve-server): Master gameplay server, domain service handlers, living world director, and ESI OpenAPI routes.

---

## Wiki Directory

### [Universe, Space & Celestial Navigation](file:///Users/sergeya/ai/EvoMu/docs/wiki/universe_and_navigation/)
* [**Pochven Navigation, Filaments & Anchoring**](file:///Users/sergeya/ai/EvoMu/docs/wiki/universe_and_navigation/pochven_navigation_and_anchoring.md): Clades, conduits, standing locks (+6.0 Home systems), filament pools, 30 km wormhole offset, and citadel ban.
* [**Stargate Jumps, Cynosural Navigation & Jump Fatigue**](file:///Users/sergeya/ai/EvoMu/docs/wiki/universe_and_navigation/stargate_jumping_and_cynosural_navigation.md): Stargate transitions, 10s session change locks, 60s gate cloaks, cyno beacon taxonomy (standard, covert, industrial), jump drive range/fuel formulas, Titan/Black Ops bridging & conduits, and Phoebe jump fatigue math.
* **Solar Systems & Region Topology:** Coordinates, security status math, K-space, J-space wormhole rules, Pochven, and Zarzakh.
* **Filaments & Abyssal Deadspace:** Tier 1–6 filaments, environmental weather penalties, gate locks, and 20m boundary destruction.

### [Ballpark Simulation & Destiny Physics Engine](file:///Users/sergeya/ai/EvoMu/docs/wiki/ballpark_and_physics/)
* [**Destiny Physics Simulation Loop & Kinematics**](file:///Users/sergeya/ai/EvoMu/docs/wiki/ballpark_and_physics/destiny_simulation_loop.md): Deterministic 1 Hz tick loop, sub-warp velocity integration ($v(t)$), align time math, 13 `DSTBALL_*` modes, 5-phase warp pipeline, and drag/catch bubbles.
* **Movement Modes:** `Stop`, `Follow`, `Orbit`, `Approach`, `Warp` kinematics and surface distance math.
* **Warp Disruption Bubbles:** Deployable warp disruptors, interdiction probes, and drag/catch bubble geometry.

### [Upwell Structures, Starbases & Orbital Deployment](file:///Users/sergeya/ai/EvoMu/docs/wiki/upwell_and_structures/)
* [**Upwell Structures Overview & Lifecycle**](file:///Users/sergeya/ai/EvoMu/docs/wiki/upwell_and_structures/upwell_structures_overview.md): Taxonomy, corporate ownership, 15m/30m reinforcement timers, damage caps, and spatial constraints.
* **Power States & Fuel Logistics:** Full Power vs. Low Power states, service fuel burn, and the 7-day Abandoned state.
* **Quantum Cores:** Gating on structure online, 100% destruction drop rate, and market sinks.
* **Access Control Lists (ACLs) & Profiles:** Access groups, member/manager roles, docking permissions, and tax brackets.
* **Asset Safety System:** Wrap creation, 5-day delivery, 20-day auto-delivery to nearest NPC station, and wormhole exceptions.
* **Pochven Anchoring Restrictions:** Total citadel ban and the Triglavian +1.5 standing gate on Customs Offices.

### [Living World, Ecology & NPC AI Systems](file:///Users/sergeya/ai/EvoMu/docs/wiki/npcs_and_living_world/)
* [**NPC Combat AI & Living World Ecology**](file:///Users/sergeya/ai/EvoMu/docs/wiki/npcs_and_living_world/npc_combat_ai_and_ecology.md): 3-tier LOD simulation, 1 Hz combat AI state machine, dynamic target switching, and optimal orbits.
* **Regional Belt Rats & Elite Spawns:** Guristas, Blood Raiders, Serpentis, Angels, Clone Soldiers (Security Tags 1206), and Officers.
* **Law Enforcement & Police Response:** Faction Police outlaws tracking ($\le -5.0$), CONCORD response and death pipeline, and Sentry Guns.
* **Living World Autonomous Convoys:** Haulers, mining fleets, route traversal, interdiction, and dynamic market price impacts.

### [Missions, Agent Divisions & PvE Encounters](file:///Users/sergeya/ai/EvoMu/docs/wiki/missions_and_agents/)
* [**Agent Dialogue, Missions & LP Stores**](file:///Users/sergeya/ai/EvoMu/docs/wiki/missions_and_agents/agent_dialogue_and_missions.md): Security, Mining, and Distribution agents; mission state machine; decline penalties; 16-mission storyline tally; LP store matching.
* **Standing Mathematics:** Faction, Corp, and Agent standing scales ($-10.0$ to $+10.0$), derived standings, and Social skill modifiers.
* **Epic Arcs & AIR NPE:** Narrative arcs, permanent standing rewards, and AIR Career Program milestone tracking.

### [Exploration, Scanning, Hacking & Relics](file:///Users/sergeya/ai/EvoMu/docs/wiki/exploration_and_hacking/)
* [**Exploration Scanning, D-Scan & Hacking Minigame**](file:///Users/sergeya/ai/EvoMu/docs/wiki/exploration_and_hacking/hacking_minigame_and_sites.md): Cone scanning (5° to 360°), 8-probe formation deviation reduction, 24-hex axial node hacking grid, virus coherence/strength, subsystems, and Ghost site detonation.
* **Directional Scanner (D-Scan) & Probes:** Cone scanning, Core/Combat probe formations, signal deviation, and signature resolution.
* **Hacking Minigame & Archaeology:** Virus coherence, virus strength, defensive subsystems, and can scattering.
* **Sleeper Caches & Ghost Sites:** Environmental puzzle triggers, timed detonation cycles, and covert research components.

### [Mining, Harvesting & Resource Processing](file:///Users/sergeya/ai/EvoMu/docs/wiki/mining_and_harvesting/)
* [**Mining Cycles, Residue & Resource Processing**](file:///Users/sergeya/ai/EvoMu/docs/wiki/mining_and_harvesting/mining_cycles_and_residue.md): Mining laser cycle engine, yield formulas, crystal volatility, waste/residue probabilities, and specialized holds.
* **Ice, Gas & Moon Harvesting:** Ice harvester cycle times, Gas Cloud harvesting, moon drilling extraction schedules, and chunk fracturing.
* **Ore & Gas Compression:** In-space industrial compression (Porpoise, Orca, Rorqual) and refinery structure services.
* **Reprocessing & Mining Ledgers:** Refining efficiency formulas, scrapmetal processing, and character/corp mining ledgers.

### [Dogma Attribute Engine, Fitting & Module Cycles](file:///Users/sergeya/ai/EvoMu/docs/wiki/dogma_and_fitting/)
* [**Dogma Attribute Engine, Modifier Graphs & Fitting**](file:///Users/sergeya/ai/EvoMu/docs/wiki/dogma_and_fitting/dogma_attribute_engine.md): Dynamic attribute modifier graph with 9 evaluation stages, non-linear stacking penalty decay ($S(i)$), differential capacitor recharge ($\frac{dC}{dt}$), Nosferatu relative % cap, and rack thermodynamics.
* **Ship Fitting & Slot Layouts:** Hi/Med/Lo slots, Rigs, Subsystems, PG/CPU budgets, and Calibration limits.
* **Module Cycles & Energy Warfare:** Module cycle ticks, capacitor draw, Nosferatu relative capacitor rules, and Neutralizers.
* **Overheating & Thermodynamics:** Heat generation per rack, damage propagation, and module burnout states.
* **Electronic Warfare (EWAR) & Propulsion:** Webs, scrams, target painters, damps, ECM, afterburners, and MWD bloom.

### [Combat Resolution, Damage & Crimewatch](file:///Users/sergeya/ai/EvoMu/docs/wiki/combat_and_crimewatch/)
* [**Gunnery Tracking, Missile Kinematics & Damage Application**](file:///Users/sergeya/ai/EvoMu/docs/wiki/combat_and_crimewatch/gunnery_tracking_and_missile_kinematics.md): Turret tracking formulas, angular velocity, quality bands & wrecking shots, missile DRF, exotic weapons (Entropic & Vorton), passive shield recharge peak at 25%, and death pipeline killmails.
* [**Crimewatch, Aggression Rules & Engagement Timers**](file:///Users/sergeya/ai/EvoMu/docs/wiki/combat_and_crimewatch/crimewatch_and_engagement_timers.md): Weapons timer (60s), PvP timer (15m), Suspect/Criminal flags (15m), Safety interlocks, and Kill Rights.

### [Industry, Manufacturing, Science & Reactions](file:///Users/sergeya/ai/EvoMu/docs/wiki/industry_and_manufacturing/)
* [**Blueprints, Manufacturing, Science & Reactions**](file:///Users/sergeya/ai/EvoMu/docs/wiki/industry_and_manufacturing/blueprints_manufacturing_and_reactions.md): BPO vs. BPC, ME/TE formulas, facility rig scaling, System Cost Index (SCI), T2 invention, and refinery reactions.
* **Science, Research & Invention:** Copying jobs, ME/TE research, T2 invention probabilities, and reverse engineering.
* **Reactions:** Composite reactions (moon materials), biochemical reactions (boosters), and hybrid reactions (T3 components).

### [Planetary Industry (PI)](file:///Users/sergeya/ai/EvoMu/docs/wiki/planetary_industry/)
* [**Planetary Industry, Colony Networks & Customs Offices**](file:///Users/sergeya/ai/EvoMu/docs/wiki/planetary_industry/colony_pin_networks_and_pi.md): 8 planet types, spherical harmonic concentration fields, Command Center upgrades, ECUs, P1–P4 production, links, and POCO tax brackets.
* **Colony Pin Networks:** Extractor Control Units (ECUs), processors, storage, spaceports, and routing links.
* **Production Tiers:** Raw materials $\rightarrow$ P1 (Basic) $\rightarrow$ P2 (Refined) $\rightarrow$ P3 (Specialized) $\rightarrow$ P4 (Advanced).
* **Customs Offices (POCOs) & Taxes:** Export/import taxation, standing tax brackets, and planetary launches.

### [Player Economy, Markets & Contracts](file:///Users/sergeya/ai/EvoMu/docs/wiki/economy_and_markets/)
* [**Regional Market Engine, Order Matching & Contracts**](file:///Users/sergeya/ai/EvoMu/docs/wiki/economy_and_markets/regional_market_order_matching.md): CLOB double auction, Maker vs. Taker price precedence, Broker fees, Accounting tax, immediate escrow, OHLCV history, and Item Exchange / Courier contracts.
* **Contracts System:** Item exchange, auctions, courier collateral, and escrow locks.
* **Trade Goods & Sinks:** NPC buy/sell commodities, Sleeper Blue Loot, Triglavian Red Loot, and Overseer Effects.

### [Corporations, Alliances & Social Governance](file:///Users/sergeya/ai/EvoMu/docs/wiki/corporations_and_governance/)
* [**Corporation Roles, Governance & Asset Management**](file:///Users/sergeya/ai/EvoMu/docs/wiki/corporations_and_governance/corporation_roles_and_governance.md): Corp founding, 24-hour role grant delay security invariant, 7 corporate hangars & wallets, shares, shareholder voting, and projects.
* [**Alliances, War Declarations & War HQ Mechanics**](file:///Users/sergeya/ai/EvoMu/docs/wiki/corporations_and_governance/alliances_and_formal_wars.md): Alliance charters, 1B ISK registration fee, high-sec War HQ physical anchoring requirement, 24h war spool-up, surrender treaties (14d peace), and defender allies.
* **Communications & Mail:** EVE Mail, mailing lists, chat channels, and fleet command hierarchies.

### [Sovereignty, Faction Warfare & Territorial Conflict](file:///Users/sergeya/ai/EvoMu/docs/wiki/sovereignty_and_faction_warfare/)
* [**Faction Warfare, Pirate Insurgencies & Encounter Surveillance System (ESS)**](file:///Users/sergeya/ai/EvoMu/docs/wiki/sovereignty_and_faction_warfare/faction_warfare_and_insurgencies.md): Frontlines, Command & Rearguard graph topology, Advantage multipliers, Havoc Pirate FOBs, Corruption/Suppression stages 1-5, low-sec bubble release, Dynamic Bounty System (DBS), and ESS Main/Reserve Bank hacking.
* [**Sovereignty Hubs, Equinox Skyhooks & Entosis Mechanics**](file:///Users/sergeya/ai/EvoMu/docs/wiki/sovereignty_and_faction_warfare/sovereignty_hubs_and_equinox_skyhooks.md): Unified Equinox Sovereignty Hub (Type 32458), Entosis warmup/capture scaling with ADM (1.0x to 6.0x), planetary Power/Workforce/Reagents, Orbital Skyhooks (Type 81080), and Siphon Bay raiding.

### [Character Progression, Skills & Medical Clones](file:///Users/sergeya/ai/EvoMu/docs/wiki/character_and_progression/)
* [**Character Progression, Skill Training & Medical Clones**](file:///Users/sergeya/ai/EvoMu/docs/wiki/character_and_progression/skill_training_and_medical_clones.md): Attribute training speed ($SP/\text{min}$), skill rank formulas ($250 \cdot R \cdot 2^{2.5(L-1)}$), Alpha vs. Omega tiers, medical clone respawn, jump clone 24h cooldown, and cybernetic implants.
* **Skills, Queues & Attributes:** Primary/secondary attributes, training rate formulas ($SP/\text{min} = \text{Primary} + \frac{\text{Secondary}}{2}$), and injectors.
* **Medical Clones & Implants:** Clone death respawn, jump clone timers, attribute implants, hardwiring implants, and pirate implant sets.
* **Booster Biology & Neurotoxins:** Combat boosters, duration scaling, and side-effect roll math.

### [Protocol Architecture, MachoNet, Quasar & SDE](file:///Users/sergeya/ai/EvoMu/docs/wiki/protocol_and_infrastructure/)
* [**MachoNet Protocol, Quasar Services & Wire Infrastructure**](file:///Users/sergeya/ai/EvoMu/docs/wiki/protocol_and_infrastructure/machonet_wire_and_quasar_gateway.md): 4-byte LE framing, GPS wire handshake state machine, CRC-CCITT placebo hash, PyRep marshal codec with inverted dict serialization, DBRow/CRowset descriptors, Quasar Protobuf gateway, and ESI OpenAPI routing.
* **Quasar Modern Services:** HTTP/2 / gRPC services, protobuf schema catalogs, and client event streaming.
* **ESI OpenAPI & SDE Pipeline:** Swagger OpenAPI routes, authentication, token validation, FSD binary parsing, and SQLite schemas.
