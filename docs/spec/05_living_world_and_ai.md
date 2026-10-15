# EvoMu Deep-Dive Spec 05: Universe Seeding, Autonomous Living Economy & Dual-Tier NPC AI (`§3.9`–`§3.10`)

> Part of the [`PLAN.md`](../../PLAN.md) Master Specification.

---

### 3.9 Universe Seeding, Daily Downtime Lifecycle & Autonomous Living Market / Economy Engine

A private or small-cluster EVE server feels static if trade hubs are empty except for NPC skillbooks, stargates have zero traffic, asteroid belts never regenerate, and System Cost Indices (`SCI`) or Bounty Risk Modifiers (`BRM`) are frozen. `EvoMu` specifies both **Static/Downtime Universe Seeding** (`crates/eve-server/src/world_seeder/`) and a bounded, deterministic **Autonomous Capsuleer Economy & Living Traffic Engine** (`crates/eve-server/src/living_world/`).

#### 1. Cluster Bootstrap & Daily Downtime Lifecycle (`11:00 UTC` / Configurable Cron, `Server_downtime.md`)
- **Static Universe Bootstrap (`eve-sde` $\rightarrow$ `eve-db`):**
  - Seeds all `8,000+` solar systems (`systems.static`), stargates (`jumps.static`), planets/moons/asteroid belts (`solarsystemcontent.static`), NPC stations (`stations.static`), NPC corporations (`npccorporations.fsdbinary`), Factions (`factions.fsdbinary`), Agents in stations & space (`agentsinspace.fsdbinary`), Starbase Moons, and Jove Observatories (`Jove_Observatory.md` — present in 500+ K-space systems).
- **Daily Downtime Maintenance Tick (`11:00 UTC` or live rolling tick without disconnecting players):**
  1. **Asteroid Belt Regeneration (`asteroidcountbytypeidbeltid.static`, `asteroidtypesbysolarsystemid.pickle`):**
     - Replenishes mined-out asteroids across every system's Asteroid Belts according to true security class ($1.0\text{ to }0.5$ Highsec: Veldspar, Scordite, Pyroxeres, Plagioclase; $0.4\text{ to }0.1$ Lowsec: Omber, Kernite, Jaspet, Hemorphite, Hedbergite, Dark Ochre, Gneiss, Ytirium; $\le 0.0$ Nullsec/W-space: Bistot, Arkonor, Crokite, Spodumain, Mercoxit, Kylixium, Griemeer, Nocxite, Hezorime, Ueganite; Pochven: Bezdnacine, Rakovene, Talassonite) and expands remaining unmined asteroids in low-traffic belts up to their maximum volume cap.
  2. **Ice Belt & Ore Anomaly Respawn Timers:**
     - Ice Belts (`Clear Icicle`, `Glacial Mass`, `Blue Ice`, `White Glaze`, `Glare Crust`, `Dark Glitter`, `Gelidus`, `Krystallos`) respawn **4 hours** after full depletion (tracked via `characterMiningLedger` / `corpMiningLedger`).
  3. **Cosmic Anomaly & Cosmic Signature Pool Maintenance (`dungeons.fsdbinary`, `dungeonExplorationMgr`):**
     - Each constellation maintains a target pool of Combat Anomalies (`Hideaway`, `Burrow`, `Refuge`, `Den`, `Yard`, *Rally Point*, `Port`, `Hub`, `Haven`, `Sanctum`, `Drone Patrol/Horde`), Ore Anomalies, Relic/Data Signatures, Gas Nebulae (`Cytoserocin`, `Mykoserocin`, `Fullerites`), Wormhole Connections (`wormholeMgr`), and **Phased Asteroid Fields** (`Phased_Asteroid_Field.md`). When a site despawns in System $A$, it immediately respawns in another system within the same constellation/region band.
  4. **Dynamic Resource Distribution (`dynamicResourceCacheMgr`, `essMgr`):**
     - **Bounty Risk Modifier (`BRM`):** Updates each Nullsec system's `BRM` ($100\%\text{ to }200\%$ equilibrium window; decays toward floor when ratting volume exceeds regional threshold, recovers toward equilibrium when quiet or when PvP ship destruction occurs in system).
     - **Encounter Surveillance System (`ESS` Main & Reserve Bank, `dynamicresources/common/ess/formulas.pyj`):**
       - Ratting bounties split between direct wallet ticks and the system's **ESS Main Bank** (auto-pays out to contributing pilots every **3 hours** unless stolen via a 5-minute link by a cruiser+ inside the $75\text{ km}$ deadspace bubble where Warp/MWD/MJD/Cloak are disabled) and **ESS Reserve Bank** (unlocked for 15 minutes or 45 minutes using **SCCEncryptedKeys** dropped in Lowsec Quantum Flux sites, paying out per pulse via `GetPayout(currentPulse, totalPulses, windowSize, windowAccessFactor, reserveBankMaxPayout, exponentLead, exponentTail, payoutPaddingValue)`).

#### 2. Authentic NPC Market Seeding (`marketProxy`, `Skillbook_Program_Seeded_Price.md`, `Quantum_Cores.md`)
At server bootstrap and on a 15-minute replenishment cycle, NPC Corporations automatically maintain infinite-depth (365-day duration) Buy and Sell orders in their stations:
1. **Skillbooks (`Skillbook_Program_Seeded_Price.md`):**
   - Standard Empire/ORE skillbooks are seeded as NPC Sell Orders at School/Academy stations at `basePrice`, and can also be purchased remotely at any time via `skillMgr2` (**Direct Skill Injection** at $+30\%$ markup over `basePrice`).
   - Rare/Faction/Precursor/T2 Specialization skillbooks are *not* NPC-seeded on the market; they drop exclusively from Exploration Relic/Data cans, LP Stores, Abyssal caches, or DED complexes.
2. **Tech I Blueprint Originals (`BPOs`) & Reaction Formulas (`blueprints.static`):**
   - All Tech I Ship, Module, Rig, Charge, Drone, Deployable, and Structure BPOs (`0% ME / 0% TE`, infinite runs) and Biochemical/Composite/Hybrid Reaction Formulas are seeded by their creator NPC corporations at `basePrice`.
3. **Upwell Structure Quantum Cores (`Quantum_Cores.md`):**
   - Sold at fixed NPC prices by the 5 Empire Navy/Security corporations (*CONCORD*, *Amarr Navy*, *Caldari Navy*, *Federation Navy*, *Republic Fleet*) — e.g., $600\text{M ISK}$ (Astrahus), $700\text{M ISK}$ (Athanor/Raitaru), $1.5\text{B ISK}$ (Azbel), $2.5\text{B ISK}$ (Fortizar/Tatara), $10\text{B ISK}$ (Sotiyo), $30\text{B ISK}$ (Keepstar) — and **bought back** by persistent NPC Buy Orders at $70\%–100\%$ of base value across Empire stations.
4. **NPC Trade Goods, Blue Loot, Red Loot & Overseer's Personal Effects (`NPC_trade_goods.md`):**
   - **Fixed NPC Sink Buy Orders:** CONCORD buys **Sleeper Data Libraries ("Blue Loot")** at $50\text{k}–10\text{M ISK}$; DED buys **Triglavian Survey Databases ("Red Loot")** at $100\text{k ISK}$ and **1st–23rd Tier Overseer's Personal Effects** at $100\text{k}–134\text{M ISK}$.
   - **Dynamic Supply/Demand Consumer Trade Goods:** Producer NPC stations sell commodities (*Dairy Products*, *Consumer Electronics*, *Robotics*, *Holoreels*, *Liquor*) along a dynamic price curve that rises as stocks are bought and decays back toward minimum as stock regenerates; Consumer NPC stations post dynamic Buy Orders whose price falls as players fill them.
5. **Item Traders (`itemTrader`, `itemtraderrecipes.fsdbinary`) & Contraband (`contrabandtypes.fsdbinary`):**
   - Station and space **Item Traders** exchange multi-input item recipes + ISK fees for rewards (Triglavian suits, event boosters, Nexus chips, Strongboxes).
   - **Contraband Items** (*Slaves*, *Small Arms*, *Nerve Agents*, *Narcotics*, *Synth Crash/Drop/Exile/Frentix/Mindflood/Sooth Sayer/X-Instinct*) are checked by `contrabandtypes.fsdbinary` against the local solar system's faction when scanned by **Empire Customs Officers** on stargates.

#### 3. Autonomous Simulated Capsuleer Economy, LOD Resource Budget & Living Traffic Engine (`living_world/`)
To make **EvoMu** feel like a populated MMO universe even with 1 to 50 human players, `EvoMu` specifies a deterministic, database-backed **Virtual Capsuleer Economy & Traffic Engine** (configured in `evomu.toml` under `[living_world]`):

1. **Virtual Trade Hub Market Makers (Jita 4-4, Amarr VIII, Dodixie IX-20, Rens VI-8, Hek VIII-12):**
   - Background economic agents maintain two-sided order books (Bid/Ask spreads, volume depth, and 365-day historical OHLCV price/volume charts for the Market History graph) across **player-manufactured items**: Raw Ores, Ice Products, Refined Minerals (*Tritanium* through *Morphite*), PI Commodities (`P0`–`P4`), Moon Materials (`R4`–`R64`), Tech I/II/III Ships, Meta 1–4 Modules, Tech II Modules/Rigs, Faction/Deadspace modules, Mutaplasmids, Implants, Boosters, Skill Injectors, and PLEX.
   - **Fair Price & Elasticity Model:** Base prices are anchored to fundamental **Material Build Cost ($\sum \text{Mineral/Component Inputs} \times \text{ME } 0.90 + \text{Job Cost}$) + Risk/Rarity Premium** for manufactured goods, and **Drop Rate $\times$ Demand Weight** for Meta/Faction/Deadspace loot.
   - **Consumption of Player Output:** When human players sell mined ore, PI goods, salvaged materials, exploration loot, or manufactured ships to Buy Orders (or list competitive Sell Orders), Virtual Industrialist/PvP Agents probabilistically purchase player Sell Orders (if priced within the fair-value band) and replenish consumed Buy Orders over time.
2. **Dynamic System Cost Index (`SCI`) & Industry Activity Simulation:**
   - Virtual Industrialists install background Manufacturing, ME/TE Research, Copying, Invention, and Reaction jobs proportional to regional traffic, giving Jita/Perimeter high `SCI` ($8\%–12\%$) and quiet backwater systems low `SCI` ($0.5\%–2\%$), rewarding players who haul materials to quiet industrial systems.
3. **Physical Stargate & Station NPC Hauler / Patrol Traffic:**
   - Active solar systems spawn physical, warp-capable **NPC Industrial Convoys** (*Badger*, *Tayra*, *Bestower*, *Sigil*, *Iteron Mark V*, *Mammoth*, *Wreathe*, *Charon*, *Obelisk*, *Fenrir*, *Nomad*), **Empire Navy Patrols**, and **Mining Fleets** that undock from stations, warp between stargates, jump across systems (`DoDestinyUpdate::JumpOut`), and dock at destination stations.
   - NPC Haulers carry cargo manifests drawn from regional trade flows, allowing player gankers or pirates in Lowsec/Highsec to cargo-scan and intercept convoys (subject to CrimeWatch/CONCORD/Standing consequences).
4. **Engineering Constraints: 3-Tier `SystemActor` Level-of-Detail (LOD), Deterministic Seeding & Inflation Bounds:**
   - **3-Tier `SystemActor` Tick LOD (CPU & Memory Budget across `8,000+` Systems):**
     - *Tier A — Active Systems ($\ge 1$ connected human player or active combat grid):* Full $1\text{ Hz}$ `Ballpark` physics and AI tick ($< 0.5\text{ ms/tick}$ target CPU budget per active system).
     - *Tier B — Transit Corridor Systems (0 human players, $\ge 1$ active NPC Hauler convoy):* Coarse $0.1\text{ Hz}$ ($10\text{s}$) waypoint-to-stargate timer transition with zero $O(N^2)$ sub-warp kinematics or bubble intersection math; promotes immediately to Tier A on player entry.
     - *Tier C — Hibernating Systems ($\sim 7,800+$ empty systems):* $0\text{ Hz}$ idle state (`0` scheduled tokio timers); anomaly respawns, belt regeneration, `SCI`, and `BRM` decay are computed lazily via analytical timestamps on entry or during the 15-minute regional economy batch tick. Total cluster memory footprint target is $< 512\text{ MB}$ RSS with $< 2\%$ single-core idle CPU.
   - **Deterministic PRNG Seeding & Anti-Inflation Invariants:**
     - All market maker order generation, price walks, and convoy manifests are driven by `ChaCha8Rng::seed_from_u64(config.universe_seed ^ tick_epoch)` so tests (`E2E-16`) are 100% deterministic.
     - **Price & Money-Supply Guardrails:** Virtual Market Makers operate against a bounded virtual reserve pool and hard price clamps $P(t) \in [0.50 \times C_{\text{build}},\ 2.50 \times C_{\text{build}}]$, with a maximum per-tick price step of $\pm 5\%$ and bounded order book depth (`max_virtual_orders_per_type = 16`). Property tests in `eve-db` / `living_world` assert that 1,000 simulated days of zero-player and adversarial-player trading never breach the $[0.50, 2.50] \times C_{\text{build}}$ band or overflow `IskAmount`.
   - **`evomu.toml` `[living_world]` Configuration Schema:**
     ```toml
     [living_world]
     enabled = true
     universe_seed = 0x4556_4F4D_5532_3431
     economy_tick_interval_secs = 900       # 15-minute regional market & SCI/BRM batch tick
     downtime_cron_utc = "0 11 * * *"       # 11:00 UTC rolling belt/anomaly regeneration
     max_active_convoys = 120               # Global cap on Tier B/A NPC Hauler convoys
     max_virtual_orders_per_type = 16       # Bid/Ask depth cap per trade hub per typeID
     price_elasticity = 0.15                # Sensitivity to player net buy/sell volume
     fair_value_band = [0.50, 2.50]         # Hard min/max multiplier relative to SDE build cost
     ```

---

### 3.10 Complete NPC Taxonomy, Faction Profiles, Spawning Mechanics & Dual-Tier AI Engine (`eve-destiny` + `behaviors/`)

EVE Online V24.01 uses **two distinct NPC AI architectures** side-by-side:
1. **Tier 1 — Classic Dogma-Attribute State-Machine AI (`NPCAI`):** Drives classic Asteroid Belt rats, Gate rats, Mission pocket rats, Deadspace/Anomaly waves, Sentry Guns, and CONCORD.
2. **Tier 2 — Behavior-Tree & Multi-Scope Blackboard AI (`behaviors/` — `178` shared `.pyj` modules in `code.ccp`):** Drives Diamond (`♦`) NPCs, Pirate Strongholds (`FOBs`), NPC Mining Operations & Response Fleets, Drifters (`Vigilant Tyrannos`), Autothysian Lancers, Triglavian Collective & EDENCOM Roaming/Defense Fleets, Sansha Incursions, Anomic Burners, Homefront Operations, and Abyssal Deadspace.

#### 1. Tier 1: Classic Dogma-Attribute State-Machine AI (`NPCAI.cpp`, `AttributeEnum.h`, `typedogma.fsdbinary`)
- **8 Finite States:** `Idle (1)`, `Chasing (2)`, `Following (3)`, `Engaged (4)`, `Fleeing (5)`, `Signaling (6)`, `WarpOut (7)`, `WarpFollow (8)`.
- **Dogma-Driven Parameters (Read dynamically from each NPC's `typeID` in `typedogma.fsdbinary`; attribute IDs verified against `AttributeEnum.h`):**
  - **Propulsion & Positioning:** `AttrMaxVelocity` (`37` — orbit velocity), `AttrEntityFollowRange` (`257`), `AttrEntityFlyRange` (`416` — preferred orbit distance), `AttrEntityCruiseSpeed` (`508` — MWD chase velocity), `AttrEntityChaseMaxDelay` (`580`), `AttrEntityChaseMaxDelayChance` (`581`), `AttrEntityChaseMaxDuration` (`582`), `AttrEntityChaseMaxDurationChance` (`583`), `AttrEntityMaxWanderRange` (`584`), `AttrEntityChaseMaxDistance` (`665` — distance threshold before switching from Orbit to MWD Chase).
  - **Turret & Missile Weapons:** `AttrSpeed` (`51` — cycle time ms), `AttrMaxRange` (`54`), `AttrDamageMultiplier` (`64`), `AttrFalloff` (`158`), `AttrTrackingSpeed` (`160`), `AttrEntityAttackRange` (`247`), `AttrEntityAttackDelayMin` (`475`), `AttrEntityAttackDelayMax` (`476`), `AttrEntityKillBounty` (`481`), `AttrEntityDefenderChance` (`497`), `AttrMissileLaunchDuration` (`506`), `AttrEntityMissileTypeID` (`507`), `AttrOptimalSigRadius` (`620`).
  - **Local Active Tank:** `AttrEntityArmorRepairDuration` (`630`), `AttrEntityArmorRepairAmount` (`631`), `AttrEntityShieldBoostDuration` (`636`), `AttrEntityShieldBoostAmount` (`637`), `AttrEntityArmorRepairDelayChance` (`638` / `AttrEntityArmorRepairDelayChanceSmall` `666`), `AttrEntityShieldBoostDelayChance` (`639` / `AttrEntityShieldBoostDelayChanceSmall` `663`).
  - **Electronic Warfare (`EWAR`) Cycle Rolls:** Every EWAR cycle checks range and rolls probability against:
    - Warp Disruption/Scrambling: `AttrWarpScrambleRange` (`103`), `AttrWarpScrambleStrength` (`105`), `AttrEntityWarpScrambleChance` (`504`), `AttrWarpScrambleDuration` (`505`).
    - Stasis Webification: `AttrSpeedFactor` (`20`), `AttrModifyTargetSpeedChance` (`512`), `AttrModifyTargetSpeedDuration` (`513`), `AttrModifyTargetSpeedRange` (`514`).
    - ECM Target Jamming: `AttrEntityTargetJam` (`928`), `AttrEntityTargetJamDuration` (`929`), `AttrEntityTargetJamDurationChance` (`930`), `AttrEntityTargetJamMaxRange` (`936`).
    - Sensor Dampening, Tracking/Guidance Disruption, Target Painting, and Energy Neutralization: `AttrEntityCapacitorDrainDurationChance` (`931`), `AttrEntitySensorDampenDurationChance` (`932`), `AttrEntityTrackingDisruptDurationChance` (`933`), `AttrEntityTargetPaintDurationChance` (`935`), `AttrEntityCapacitorDrainMaxRange` (`937`), `AttrEntityCapacitorDrainDuration` (`942`), `AttrEntityCapacitorDrainAmount` (`946`).
  - **Sleeper / Advanced Target-Switching Attributes:** `AttrAI_ShouldUseTargetSwitching` (`1648`), `AttrAI_ShouldUseSecondaryTarget` (`1649`), `AttrAI_ShouldUseSignatureRadius` (`1650`), `AttrAI_ChanceToNotTargetSwitch` (`1651`), `AttrAI_ShouldUseEffectMultiplier` (`1652`), `AttrAI_ImmuneToSuperWeapon` (`1654`), `AttrAI_PreferredSignatureRadius` (`1655`).

#### 2. Tier 2: Behavior-Tree & Multi-Scope Blackboard Engine (`behaviors/` — `178` Shared `.pyj` Modules in `code.ccp`)
> **Provenance & Clean-Room Implementation Note:**
> The `behaviors/` package (`178` `.pyj` modules) is **shared client/server Python bytecode shipped inside the V24.01 client's `code.ccp` archive** (used by client-side authoring/debug tools and node-graph inspectors). Disassembling `behaviors/` provides the authoritative specification of composite node types, blackboard scope keys, tactical role names, and target evaluators, while the server-side runtime execution in `EvoMu` is implemented in clean-room Rust inside `crates/eve-destiny` and `crates/eve-server`.

- **Behavior Tree Composites, Decorators & Monitors (`behaviors/behaviortree.pyj`, `behaviors/composites/`, `behaviors/decorators/`, `behaviors/monitors/`):**
  - Composites: `Sequence`, `PrioritySelector`, `WeightedSelector` (`behaviors/composites/weighted.pyj`), `Parallel`, `ReferenceSubtree`.
  - Decorators: `RepeatWhileFailing`, `CooldownTimer`, `Uninterruptible`, `ForceFailure`, `Modifiers`.
  - Monitors (`behaviors/monitors/`): `abyssal_deadspace`, `ballparks`, `blackboards`, `combat`, `combattargets`, `damage`, `dungeons`, `effects`, `entities`, `groups`, `logistics`, `messages`, `navigation`, `reinforcements`, `standings`, `superweapon`, `targeting`, `timers`.
- **6 Hierarchical Blackboard Scopes (`behaviors/blackboards/scopes.pyj`):**
  1. `ItemScope` (`myItemId` — individual NPC memory: `MY_COMBAT_TARGET`, `WARP_SCRAMBLED`, `CORPSE_ITEM`, `SUPER_WEAPON_READY`).
  2. `EntityGroupScope` (`myEntityGroupId` — squad/fleet memory: `COMMANDER`, `GROUP_PRIMARY_TARGET`, `COMBAT_TARGETS_SET`, `COMBAT_MIN_OPTIMAL_RANGE`, `COMBAT_MIN_FALLOFF_RANGE`, `HAULER_PICK_UP_BALLS`, `HAULER_REQUESTED`, `REINFORCEMENT_REQUESTED`, `LAST_REINFORCEMENT_REQUEST_COORDINATE`).
  3. `DungeonScope` (`myDungeonSpawnId` — anomaly/complex state).
  4. `TaleScope` (`myTaleId` — constellation/system campaign state for Incursions, FOBs, Insurgencies, Invasion systems).
  5. `SpawnPointPoolScope` (`mySpawnPointPoolId` — shared spawn pool limits).
  6. `NodeGraphScope` (`myNodeGraphInstanceId` — direct bi-directional integration with `nodegraphs.fsdbinary`).
- **10 Tactical Fleet Roles (`behaviors/const/behaviorroles.pyj`):**
  - `'commander'`, `'dps'`, `'anti-dps'`, `'anti-dps (with primary)'`, `'anti-logistic'`, `'logistic'`, `'tackler'`, `'ew'` / `'ewar'`, and `'dreadnought'`.
  - **Fleet Commander Anchoring & Promotion (`behaviors/actions/fleet.pyj`, `behaviors/actions/combatnavigation.pyj`):** Subordinate NPCs anchor on and match velocity with their group's `COMMANDER` (`COMBAT_ORBIT_COMMANDER_VELOCITY_FACTOR`) while computing group engagement envelope (`COMBAT_MIN_OPTIMAL_RANGE`, `COMBAT_MIN_FALLOFF_RANGE`). If the Commander is destroyed, a new ship is promoted to `COMMANDER` and the fleet dynamically adjusts its engagement range.
  - **On-Grid Tactical Ping-Warping:** When targets kite beyond effective range (`COMBAT_MAX_EFFECTIVE_DISTANCE_FACTOR`), the group warps out to an on-grid tactical ping ($400\text{ km}–1,500\text{ km}$) and warps directly back onto the player at optimal range.
- **9 Target Evaluators (`behaviors/targetEvaluation.pyj`, `behaviors/const/combat.pyj`, `behaviors/utility/threat.pyj`):**
  1. `ThreatTargetEvaluator` (`'threat'`): Samples `damageTracker` over `threatSampleIntervalSeconds = 10` (`GetDamageForShipSince` + `GetHealingDoneByShipSince`) to prioritize high-DPS and remote-repairing ships.
  2. `CostTargetEvaluator` (`'cost'`): Prioritizes targets by total hull + fitted module ISK value.
  3. `FittedModuleGroupTargetEvaluator` (`'module'`): Prioritizes targets fitted with `preferredModuleGroups` (e.g., `'anti-logistic'` NPCs focus player Logistics ships; `'anti-dps'` focus player siege/bastion/turret ships).
  4. `SizeTargetEvaluator` (`'size'`), 5. `SignatureRadiusAndVelocityEvaluator` (`'sigRadius & velocity'`), 6. `RangeTargetEvaluator` (`'range'`), 7. `RandomTargetEvaluator` (`'random'`), 8. `TowingTowObjectiveEvaulator` (`'towing tow game objective'`), 9. `BasicTargetEvaluator` (`'zero'`).
- **Group Threat Management & Neutral Logi Aggro (`behaviors/trees/groups/threatManagement.pyj`):**
  - Runs `OwnerListFilterAction`, `TargetsWithOwnerEnteredBubbleMonitor`, `MonitorProximitySensorsForGroupMembers`, and `MonitorAssistanceToGroupTargets` (any neutral player repairing a target in `COMBAT_TARGETS_SET` is immediately added to the group's hostile target set).

#### 3. Complete Faction Combat Profiles (Damage Dealt, Weakest Resistance & Signature EWAR, `source: typedogma.fsdbinary, NPC_damage_types.md`)
| NPC Faction / Group | Primary / Secondary Damage Dealt | Weakest / Secondary Resist Hole | Signature EWAR & Special Mechanics |
|---|---|---|---|
| **Serpentis** (`Core` / `Shadow`) | Kinetic / Thermal | Kinetic / Thermal | **Sensor Dampeners** (`-50%` targeting range / scan res) + `90%` Stasis Webs (Shadow) |
| **Guristas Pirates** (`Pith` / `Dread`) | Kinetic / Thermal (Missiles) | Kinetic / Thermal | **ECM Target Jammers** (Gravimetric/Multispectral) |
| **Blood Raiders** (`Corpus` / `Dark`) | EM / Thermal | EM / Thermal | **Energy Neutralizers / Nosferatu** + Tracking Disruption (Amarr hulls) |
| **Sansha's Nation** (`Centus` / `True`) | EM / Thermal | EM / Thermal | **Tracking Disruptors** (Optimal Range & Tracking Speed scripts) |
| **Angel Cartel** (`Gist` / `Domination`) | Explosive / Kinetic (+ EM/Therm) | Explosive / Kinetic | **Target Painters** + high-velocity MWD orbit + Stasis Webs |
| **Mordu's Legion** | Kinetic / Thermal (Fast Missiles) | Kinetic / Explosive | **Long-Range Warp Disruptors (`35+ km`)** & Warp Scramblers |
| **Rogue Drones** (`Alvus` / `Sentient`) | Omni (Kinetic/Thermal/EM/Explosive) | **EM** / Thermal | Webs, Scrams, Target Painters (Strain variants), **Zero Bounty** in missions (drop Drone Compounds/Alloys) |
| **Mercenaries** | Thermal / Kinetic | Kinetic / Thermal | Webs & Warp Scramblers |
| **Amarr Empire Navy** | EM / Thermal | EM / Thermal | Tracking Disruptors & Energy Neutralizers |
| **Caldari State Navy** | Kinetic / Thermal | Kinetic / Thermal | ECM Target Jammers |
| **Gallente Federation Navy** | Thermal / Kinetic | Kinetic / Thermal | Sensor Dampeners |
| **Minmatar Republic Fleet** | Explosive / Kinetic / Thermal | Explosive / Thermal | Target Painters & Stasis Webs |
| **Khanid Kingdom** | Thermal / Kinetic / EM | EM / Thermal | Energy Neutralizers & Tracking Disruptors |
| **Ammatar Mandate** | EM / Thermal | Explosive / Kinetic | Tracking Disruptors & Target Painters |
| **Sleepers** (*W-Space*) | Omni (Turrets + Heavy Missiles) | Omni (Equal EM/Therm/Kin/Exp) | Remote Repair, Webs, Scrams, Neuts, frequent **AI Target Switching** & Anti-Drone aggro |
| **Drifters** (`Vigilant Tyrannos`) | Omni (`Lux Kontos` Turrets + `750k` DD) | Omni (Equal Shield/Armor) | **Blue Overshield (`turboshield`)** + **`750,000` Omni Doomsday** + **Pod Hunting** + **Corpse Harvesting** |
| **Triglavian Collective** (`♦`) | Thermal / Explosive (Ramping Spool) | Kinetic / Thermal / Explosive | Remote Repairs, Energy Neuts, Target Painters, Webs, Warp Scramblers, Anti-Drone AI |
| **EDENCOM** (`♦`) | EM / Kinetic (Arcing Vorton) | Explosive / Thermal | Arcing Vorton Projectors (hits primary + **up to 9 additional targets within `10 km`**), Remote Shield Transfers |
| **Deathless Circle** | Kinetic / Thermal + **Breacher Pods** | Kinetic / Explosive | True-damage % HP/s **SCARAB Breacher Pods** |

#### 4. Complete NPC Spawn Hierarchy & Special World Encounters
1. **Standard Belt & Gate Rats (`Belt_rat.md`, `NPC_naming_conventions.md`, `Ratting.md`):**
   - **Ship Class Naming Hierarchy:** Every pirate faction uses strict hull-size prefixes (e.g., Guristas: `Pithi` Frigate $\rightarrow$ `Pithior` Destroyer $\rightarrow$ `Pithum` Cruiser $\rightarrow$ `Pithatis` Battlecruiser $\rightarrow$ `Pith` Battleship; Elite Deadspace adds `Dire`/`Arch`/`Elder`/`Guardian` prefixes).
   - **Belt Respawn & Chaining (`20-Minute` Cycle):** Asteroid Belt spawns check every `20 minutes`. If the entire spawn group is cleared, a fresh random group (rat pack, Hauler, Faction Commander, or Officer) rolls on the next cycle; if only partial ships are killed ("Belt Chaining"), the killed hulls respawn from the same group template.
2. **Pirate Hauler Spawns (`Iteron`/`Mammoth`/`Bestower`/`Badger` variants):**
   - Rare spawns in Lowsec and Nullsec asteroid belts defended by escort rats; when destroyed, drop up to **$100,000,000+\text{ ISK}$ in refined minerals** inside a Cargo Container (`27,500+ m³`).
3. **Faction Commander Spawns (`Dread Guristas`, `Shadow Serpentis`, `Dark Blood`, `True Sansha`, `Domination`, `Sentient`):**
   - Rare spawns in Asteroid Belts, Gate grids, and Combat Anomalies; carry high bounties and drop **Faction Modules, Implants, Ammunition, and 1-run Faction Ship BPCs** (*Astero*, *Stratios*, *Nestor*, *Worm*, *Gila*, *Rattlesnake*, *Daredevil*, *Vigilant*, *Vindicator*, *Cruor*, *Ashimmu*, *Bhaalgorn*, *Succubus*, *Phantasm*, *Nightmare*, *Dramiel*, *Cynabal*, *Machariel*).
4. **The 24 Named Pirate Officers (`Belt_rat.md`, `Officer` spawns):**
   - Spawn in true Nullsec (`-0.5` to `-1.0` security) Asteroid Belts, Stargates, and Sanctums (`4` named Officers per faction across `6` factions = `24` Officers: **Estamel Tharchon**, **Vepas Minimala**, **Thon Eney**, **Kaikka Peunato** for Guristas; **Cormack Vaaja**, **Setele Schellan**, **Tuvan Orth**, **Brynn Jerdola** for Serpentis; **Draclira Merlonne**, **Ahremen Arkah**, **Raysere Giant**, **Tairei Namazoth** for Blood Raiders; **Chelm Soran**, **Vizan Ankonin**, **Selynne Mardakar**, **Brokara Ryver** for Sansha; **Tobias Kruzhory**, **Gotan Kreiss**, **Hakim Stormare**, **Mizuro Cybon** for Angels; **Unit P-343554**, **Unit F-435454**, **Unit W-634**, **Unit D-34343** for Rogue Drones).
   - Accompanied by 3–4 elite battleship escorts; deal high DPS/EWAR and drop **Officer Modules (`Purple`)**.
5. **Capital, Supercarrier & Titan NPC Spawns (`Ratting.md`):**
   - Spawn in Nullsec Belts and high-tier Combat Anomalies:
     - **NPC Dreadnoughts & Carriers:** $60,000,000\text{ ISK}$ bounty; require multiple points of Warp Disruption strength to prevent warping out when damaged; Carriers launch **NPC Fighter Squadrons**.
     - **NPC Supercarriers (`Revenant`/`Vendetta`/`Nyx`/`Hel`/`Aeon`/`Wyvern` variants):** $120,000,000\text{ ISK}$ bounty.
     - **NPC Faction Titans (`Vanquisher`/`Komodo`/`Molok`/`Loggerhead`/`Dagon`/`Chemosh`):** $240,000,000\text{ ISK}$ bounty; fire **NPC Doomsday Superweapons** (`superWeaponMgr`) and drop Capital Faction BPCs/Components.
6. **Lowsec Clone Soldiers & CONCORD Security Tags (`source: Security_tags.md, securityOfficeSvc`):**
   - Spawn exclusively in `0.4` to `0.1` Lowsec Asteroid Belts (1 single Battlecruiser-sized rat with a 20-minute respawn):
     - $0.4$ Sec: **Clone Soldier Trainer** (repairs $[-10.0, -8.0]$ security status at $0.5$ increment per tag + $100\text{k ISK}$).
     - $0.3$ Sec: **Clone Soldier Recruiter** (repairs $[-8.0, -5.0]$ at $+1.87\text{M ISK}$ per tag).
     - $0.2$ Sec: **Clone Soldier Transporter** (repairs $[-5.0, -2.0]$ at $+7.8\text{M ISK}$ per tag).
     - $0.1$ Sec: **Clone Soldier Negotiator** (repairs $[-2.0, 0.0]$ at $+25\text{M ISK}$ per tag).
   - Handed in at **CONCORD Security Offices** located in stations inside $0.4\text{–}0.1$ Lowsec systems via `crimewatch` (`CanAccessServiceInStation`, `RegainSecurityStatus`).
7. **NPC Mining Operations, Hauler Pickups & Threat-Scaled Response Fleets (`NPC_Mining_Operations.md`, `behaviors/trees/miners/`):**
   - Spawn in Asteroid Belts across Highsec, Lowsec, and Nullsec (both **Empire Resource Corporations** and **Pirate `♦` Guristas/Blood Raider** mining fleets):
     - Ventures (`1–5`), Procurers/Skiffs, Hulks, or Rorquals actively mine asteroids (leaving visible depleted rocks) and fill their ore holds.
     - When full, miners post `HAULER_REQUESTED` / `HAULER_PICK_UP_BALLS` to the `EntityGroup` blackboard (`behaviors/actions/hauling.pyj`), summoning an **NPC Mining Hauler** (*Iteron Mark V*, *Bestower*, *Tayra*, *Mammoth*, or *T2 Deep Space Transport / Freighter*).
     - The Hauler scoops the ore and warps to a **hidden Deadspace Safespot** in system (findable with **Combat Scanner Probes**) before despawning; destroying the Hauler drops a **Strongbox** + Refined Minerals + Mining Upgrades.
     - **Response Fleet Calculation (`behaviors/actions/reinforcements.pyj`):** If a player attacks the miners, or has negative standing ($\le -5.0$), the miners warp to their safe spot and call a **Diamond (`♦`) Response Fleet** (`tackler`, `logistic`, `anti-dps`, `anti-logistic`) whose size and hull tier scale dynamically with the total hull/EHP weight of all player ships within a **$375\text{ km}$ detection radius**.
8. **Pirate Forward Operating Bases (`FOB` / `Stronghold.md`):**
   - Guristas and Blood Raider Strongholds spawn in $0.8\text{ to }0.5$ Highsec systems as combat-probable structures ($1\text{M}$ Shield / $1\text{M}$ Armor / $1\text{M}$ Hull):
     - **Regional Threat Escalation:** While a FOB survives in a system, it spawns roaming Diamond (`♦`) Pirate Patrols on stargates/stations and Pirate Mining Fleets in belts. The longer it stays alive, the stronger its roaming patrols become.
     - **Structure Defense & Response Fleet (`behaviors/`):** The FOB itself fires $300\text{ km}$ Stasis Webifiers (`-65%` speed), Heavy Energy Neutralizers (`-1,200 GJ` every 24s), ECM, and Citadel Cruise Missiles (`200+ DPS`), and spawns a continuously reinforcing Diamond (`♦`) Defense Fleet scaled to the attacking player fleet (plus structure-repairing Logistics cruisers if attackers fall below the **$500\text{ HP/s}$ damage threshold**).
     - **Destruction Payout:** Enforces a 15-minute final Hull timer; on destruction, pays **$25,000,000\text{ to }30,000,000\text{ ISK}$ per pilot** (for up to $40$ fleet members) and drops a `20,000 m³` container with Faction Rig BPCs and Structure modules.
9. **Drifters (`Vigilant Tyrannos`), Autothysian Lancers & The 5 Drifter Wormhole Hives (`Drifters.md`, `behaviors/trees/drifters/`):**
   - **Autothysian Lancers (`behaviors/trees/scoutexplore.pyj`):** Patrol Highsec/Lowsec/Nullsec systems containing **Jove Observatories**, warping between gates, stations, and observatories and cargo-scanning players. If attacked, they summon **Apollo / Artemis Tyrannos** Drifter Battleships; if a Lancer kills the offending player's ship, they de-aggress, whereas killing a Lancer escalates to a full Drifter Battleship response that **pods the attacker (`AddCharacterForPodding`) and harvests their corpse (`corpseharvesting.pyj`)**.
   - **Drifter Battleship Mechanics (`behaviors/trees/drifters/superweapon.pyj`):**
     - `29` named `Tyrannos` identities (e.g., *Apollo*, *Artemis*, *Hikanta*, *Scylla*, *Tyrannos Strategos*, *Arithmos Tyrannos*, *Hyleus Tyrannos*).
     - Orbit at $18\text{ km}$ at $2,000\text{ m/s}$ firing `Lux Kontos` omni turrets (`1,100+ DPS`).
     - Protected by a **Blue Overshield (`turboshield` space component)**: while active, the Drifter refills its main shield layer to $100\%$ as the Overshield depletes. The exact tick the Blue Overshield reaches $0\%$ (`WaitForTurboShieldState`), the Drifter fires its **`Lux Xiphos` Doomsday Superweapon (`750,000` raw omni damage)** at its primary target.
   - **C5/C6 Wormhole Drifter Response Battleship (`Arithmos Tyrannos`) & Capital Escalation (`Hyleus Tyrannos`):**
     - Spawns at the end of C5/C6 Anomaly sites ($300\text{M ISK}$ Blue Loot drop; no Doomsday, strong `50 km` scram/web/neut). If players bring a Capital ship to the site, a second Drifter (**Hyleus Tyrannos**, $100\text{M ISK}$) spawns alongside it.
   - **The 5 Reconfigured Drifter Wormhole Hive Systems (*2025 Drifter Crisis*):**
     - **J110145** (*Conflux Eyrie* — controlled by CONCORD/EDENCOM), **J164710** (*Liberated Barbican* — Minmatar Republic), **J174618** (*Azdaja Redoubt* — Triglavian Collective), **J200727** (*Sentinel MZ* — Caldari State), and **J055520** (*Sanctified Vidette* — Amarr Empire), each containing up to 65 wormhole connections and a central **Drifter Hive** dungeon.
10. **Pochven (`27` Systems), Triglavian Minor Victories & EDENCOM Fortresses (`Pochven.md`, `Triglavian_minor_victory.md`, `EDENCOM_systems.md`):**
    - **Pochven (`Region 10000070`):** 27 former Empire systems carved into three **Krais** (*Krai Perun*, *Krai Veles*, *Krai Svarog*) arranged in a triangle of **Border**, **Internal**, and **Home** (*Niarja*, *Archeezia*, *Kino*) systems:
      - Wormhole-style **Delayed Local Chat**, **$-1.0$ Nullsec security rules**, **no Cyno beacons allowed**, **no new Structure anchoring**, **60% PvP Loot Fairy drop rate**, and **Triglavian Stargate Standing Locks** ($1.0$ Triglavian standing once unlocked, or historical $1.0 / 3.0 / 7.0$ for Border/Internal/Home conduits and station services).
      - Dynamic multi-faction warfare between **Triglavian Collective**, **EDENCOM**, **Drifters**, **Sleepers**, and **Rogue Drones** fighting on grids and wormholes (**`C729`** inbound/outbound Pochven wormholes), plus **Observatory Flashpoint** dreadnought sites ($3.5\text{B ISK}$ fleet payout), **World Ark Assaults** (*Xordazh*), **Stellar Fleet Deployment Sites**, **Overmind Nursery Groves**, and **Deepflow Rifts**.
    - **Highsec/Lowsec Triglavian Minor Victory Systems & EDENCOM Minor Victory / Fortress Systems:**
      - Triglavian Minor Victory systems apply a $-15\%$ armor repair / shield transfer weather penalty (`systemwide_dbuff`), spawn **Triglavian Werposts** on gates/stations (`250 km` instant-locking Entropic Disintegrator sentry guns that shoot anyone with negative Triglavian standing), and host **Extractive Super-Nexus** mining sites.
      - EDENCOM Fortress & Minor Victory systems apply a $+10\%$ shield HP weather bonus, spawn **EDENCOM GunStars** (Vorton sentry guns + `125 km` webifiers) and roaming EDENCOM Defense Fleets that shoot players with negative EDENCOM standing.
11. **Law Enforcement: CONCORD, Faction Police, Faction Navy, Sentry Guns & Customs (`source: CONCORD.md:70-82`):**
    - **CONCORD (`1.0` to `0.5` Highsec Illegal Aggression):**
      - Exact response time by true security status across all 3 grid states (`CONCORD.md:70-82` — faster when CONCORD is already on the same grid; slower when CONCORD has already been spawned/pulled onto another grid elsewhere in the solar system):
        | System Security | Already on Grid | Baseline (Fresh Spawn in System) | Pulled Elsewhere in System |
        |---:|---:|---:|---:|
        | `1.0` / `0.9` | **~$2\text{s}$** | **$6\text{s}$** | **~$11–12\text{s}$** |
        | `0.8` | **~$3\text{s}$** | **$7\text{s}$** | **~$12–13\text{s}$** |
        | `0.7` | **~$5\text{s}$** | **$10\text{s}$** | **~$15–16\text{s}$** |
        | `0.6` | **$8\text{s}$** | **$14\text{s}$** | **$19\text{s}$** |
        | `0.5` | **$9\text{s}$** | **$19\text{s}$** | **$24\text{s}$** |
      - **Execution Sequence:** Upon landing on grid, **CONCORD Police Frigate/Cruiser/Captain** immediately applies infinite Warp Scramble, `99%` Stasis Web, Sensor Dampener, ECM Jam, full Capacitor Zeroing, and sets the criminal's `droneBandwidth` to `0` (disconnecting all active drones), followed by **CONCORD Police Commander** firing a one-shot lethal weapon that destroys any subcapital or freighter hull regardless of EHP. Attempting to evade or warp while criminal is blocked in EVE; `EvoMu` hard-blocks warp/jump/dock the instant a Criminal flag is active in Highsec.
    - **Faction Police (Outlaw Security Status Enforcement in Highsec):**
      - Attacks players whose character Security Status is below the system threshold: $\le -2.0$ in `1.0`, $\le -2.5$ in `0.9`, $\le -3.0$ in `0.8`, $\le -3.5$ in `0.7`, $\le -4.0$ in `0.6`, and $\le -4.5$ in `0.5` (can be evaded by warping quickly in Pods/Interceptors, unlike CONCORD).
    - **Faction Navy (Low Empire Faction Standing Enforcement in Highsec):**
      - Attacks players whose Faction Standing with the system's sovereign Empire is $\le -5.0$.
    - **Station & Stargate Sentry Guns (`0.4` to `0.1` Lowsec & Highsec):**
      - Engage Suspect/Criminal aggressors within $150\text{ km}$ of Stations and Stargates (`350 DPS` kinetic/thermal + `3.5s` cycle time, no EWAR tackle).

---

