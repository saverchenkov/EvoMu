# EvoMu Deep-Dive Spec 03: Comprehensive PvE/Structure Taxonomy & V24.01 Modern Feature Parity (`§3.7` & `§3.11`)

> Part of the [`PLAN.md`](../../PLAN.md) Master Specification.

---

### 3.7 Comprehensive PvE, Exploration, Incursion, Insurgency, Zarzakh & Structure Taxonomy (SDE-Driven with `~/ai/eve-wiki` Test Oracles)

All mission definitions, dungeon rooms, Epic Arc DAGs, and structure attributes are loaded dynamically from `missions.fsdbinary`, `dungeons.fsdbinary`, `epicarcs.fsdbinary`, and `spacecomponentsbytype.fsdbinary`, while `~/ai/eve-wiki` (`4,102` articles) provides the documented behavioral specifications and test oracles:

1. **Complete Agent & Mission Taxonomy (`1,488+` Wiki Articles, `source: missions.fsdbinary, epicarcs.fsdbinary, agentsinspace.fsdbinary`):**
   - **Standard Agent Divisions:** *Security* (`709` documented missions), *Distribution* (`81` missions), *Mining* (`31` missions — Ore, Ice, and Gas harvesting objectives), and *R&D / Research* (`Research_missions.md` generating daily passive Research Points + optional daily missions).
   - **Storyline Missions (`121` articles, `Storyline_missions.md`):** Triggered automatically on every **16th regular mission** completed of the same Level ($1..5$) and Faction; routes player to the nearest Storyline Agent to grant major Faction Standing (with inter-faction Derived Standings) and Storyline Implant/Module rewards.
   - **Anomic / Burner Missions (`14` articles, `Anomic_missions.md`):** Optional Level 4 Security missions that can be declined without standing penalty, gated by strict hull-size Acceleration Gates:
     - *Anomic Agent* (Frigate gate): 1v1 against a pirate faction Frigate (*Dramiel*, *Cruor*, *Worm*, *Succubus*, *Daredevil*) with capsuleer-grade speed, EWAR, and damage.
     - *Anomic Team* (Frigate gate): 1 Empire Assault Frigate (*Vengeance*, *Hawk*, *Enyo*, *Jaguar*) backed by 2 Logistics Frigates (*Inquisitor*, *Bantam*, *Navitas*, *Burst*).
     - *Anomic Base* (Battlecruiser gate): High-intensity multi-wave pocket (*Talos* squad, *Ashimmu* + *Bhaalgorn* + *Sentinel*, *Rattlesnake* + *Kitsune*s, or *Vigilant*s).
   - **COSMOS Constellations (`334` missions, `246` agents, `87` static sites, `COSMOS.md`, `agentsinspace.fsdbinary`):** Once-per-character standing-gated agents floating in space beacons across 9 COSMOS regions/constellations (*Amarr*, *Caldari*, *Gallente*, *Minmatar*, *Angel*, *Blood Raider*, *Guristas*, *Sansha*, *Serpentis*). Require retrieving ancient artifacts from static multi-pocket COSMOS Complex sites and reward unique Storyline BPCs (*Michi's Excavation Augmentor*, *Pashan's Turret* implants, etc.).
   - **Data Center Agents (`47` missions, `23` agents) & Circle Agents (`31` missions):**
     - *Data Centers (`Data_center_missions.md`):* Instant Faction Standing turn-ins trading Pirate Dog Tags (`20x` Copper/Bronze/Silver/Gold/Platinum tags) or starter Graduation Certificates.
     - *Circle Agents (`Circle_missions.md`):* 5-to-8 agent referral loops in each of the 4 Empires offering repeatable Level 1 courier missions with a one-time Faction Standing reward.
   - **The 9 Epic Arcs (`40` chapter articles, `206` mission pages, `epicarcs.fsdbinary`, `Epic_arcs.md`):** Branching multi-chapter storylines repeatable every 90 days whose Faction Standing rewards **do not incur negative Derived Standings** and can be shared with fleetmates:
     1. *The Blood-Stained Stars* (`EPICARC_SOE = 29`, Sisters of EVE L1, 51 missions),
     2. *Penumbra* (`EPICARC_CALDARI = 40`, Caldari L4, 17 missions),
     3. *Right to Rule* (`EPICARC_AMARR = 48`, Amarr L4, 18 missions),
     4. *Syndication* (`EPICARC_GALLENTE = 52`, Gallente L4, 19 missions),
     5. *Wildfire* (`EPICARC_MINMATAR = 53`, Minmatar L4, 18 missions),
     6. *Smash and Grab* (`EPICARC_GURISTAS = 55`, Guristas L3, 12 missions),
     7. *Angel Sound* (`EPICARC_ANGEL = 56`, Angel L3, 11 missions),
     8. *Fractured Legacy* (`EPICARC_ORE = 128`, ORE L1, 3 missions), and
     9. *Vision of Greatness* (`EPICARC_AIR = 132`, AIR L1, 8 missions; full table in `Section 3.8.8`).
   - **Locator Agents (`Locator_Agents.md`):** Level 1–5 agents that locate any non-wormhole capsuleer across the cluster for a standing/ISK fee, with cooldowns ($5–30\text{ min}$) and EVEmail delivery delays ($0–8\text{ min}$) based on jump distance (same system, constellation, region, or different region).
2. **Combat Anomalies (`151`), Unrated Complexes (`82`), DED Rated Complexes (`47`) & Multi-Stage Expeditions (`95`):**
   - **Cosmic Anomalies (`Combat_Anomalies.md`, `dungeons.fsdbinary`):** Non-scannable sites (*Hideaway*, *Burrow*, *Refuge*, *Den*, *Yard*, *Rally Point*, *Port*, *Hub*, *Havens*, *Sanctums*, *Drone Horde/Patrol*). Destroying the final wave has a chance to spawn a Faction/Dreadnought/Titan NPC or trigger an **Unrated Escalation (`Expedition`)**.
   - **DED Rated Complexes (`1/10` through `10/10`, `DED_Complexes.md`) & Unrated Complexes (`Unrated_Complexes.md`):** Scannable Cosmic Signatures with strict ship-class Acceleration Gates (`1/10–2/10` Frigate/Destroyer, `3/10` Cruiser, `4/10` Battlecruiser/Battleship, `5/10–10/10` Battleship/T3). Guaranteed drop of **Overseer's Personal Effects** (Tier 1–23) + probability roll for Deadspace (`C-Type`, `B-Type`, `A-Type`, `X-Type`) modules and Pirate Ship BPCs.
   - **4-Stage Escalations / Expeditions (`Expeditions.md`):** Killing the trigger NPC/structure in an anomaly or unrated complex rolls a 1–4 stage Expedition chain across nearby systems (24-hour journal expiry per stage), culminating in a Commander/Structure boss room.
3. **Sansha Incursions (`14` mechanics articles, `16` site guides, `Incursions.md`, `Incursion_sites.md`, `taleMgr`):**
   - **Constellation Lifecycle & Influence Penalties:** Spawns across a constellation with 1 *Staging* system, 3–6 *Vanguard* systems, 1–3 *Assault* systems, and 1 *Headquarters* system. Phases transition from *Mobilizing* $\rightarrow$ *Established* $\rightarrow$ *Withdrawing* (5–8 days). High Sansha influence applies constellation-wide penalties: up to $-50\%$ NPC bounty/sec status, $-20\%$ shield/armor resistances, $-50\%$ weapon/drone damage, and system-wide Cynosural Jamming.
   - **5 Site Tiers, Fleet Size Caps & Overstacking Payout Decay (`source: Incursions.md` test oracle):**
     - *Staging Sites* (*Distress Beacon*, *Forward Reconnaissance Outpost*, *Nation Industrial Proxy*, *Propaganda Cluster*): $3,500,000\text{ ISK} + 400\text{ CONCORD LP}$ (top 3–5 contributors).
     - *Vanguard Sites* (*Nation Commander Outpost*, *Nation Mining Colony* [requires mining & delivering **Lyavite** ore], *Override Transfer Array* [requires **Hacking**]): Highsec $10,395,000\text{ ISK} + 1,400\text{ LP}$ for 5–10 pilots (overstacking decay: 11 pilots $= 9,615,375\text{ ISK}$, 12 pilots $= 8,783,775\text{ ISK}$, 13 pilots $= 7,900,200\text{ ISK}$); Low/Nullsec $15,000,000\text{ ISK} + 2,000\text{ LP}$ for 5–15 pilots.
     - *Assault Sites* (*Nation Commander Stronghold*, *Nation Consolidation Network*, *Overwhelmed Civilian Facility*): Highsec $18,200,000\text{ ISK} + 3,500\text{ LP}$ (10–20 pilots); Low/Nullsec $26,000,000\text{ ISK} + 5,000\text{ LP}$ (10–30 pilots).
     - *Headquarter Sites* (*Nation Rebirth Facility*, *True Creations Research Center*, *True Power Provisional Headquarters*): Highsec $31,500,000\text{ ISK} + 7,000\text{ LP}$ (20–40 pilots); Low/Nullsec $45,000,000\text{ ISK} + 10,000\text{ LP}$ (20–60 pilots).
     - *Mothership Sites* (*The Kundalini Manifest* in High/Nullsec; *Uroborus* in Lowsec): Spawns when constellation influence hits $0\%$. Highsec $63,000,000\text{ ISK} + 14,000\text{ LP}$ (40–80 pilots); Low/Nullsec $90,000,000\text{ ISK} + 20,000\text{ LP}$ (40–120 pilots) + **Revenant** supercarrier BPC drop in Lowsec. Destroying the Mothership ends the Incursion and disburses all banked CONCORD LP across the constellation.
4. **Homefront Operations (`9` Sites, `Homefront_operation.md`) & Mercenary Tactical Operations (`4` Sites, `Mercenary_Tactical_Operation.md`):**
   - **Homefront Operations:** Cooperative 3-pilot and 5-pilot Highsec anomalies with role-specific mechanics (*Abyssal Artifact Recovery*, *Dread Assault*, *Emergency Aid*, *Metaliminal Meteoroid*, *Raid*, *Salvage Research*, *Stabilize Rift*, *Suspicious Signal*, *Traffic Stop*) combining DPS, Logistics, Mining, Hauling, Hacking, Cargo Scanning, and Tackling.
   - **Mercenary Dens & MTOs (*Drifter Muster Point*, *Rogue Drone Emergence*, *Sansha Raiding Party*):** Anchored on Skyhook planets; tracks **Development** ($0..4$, passive $+5\text{ pts}/24\text{h}$, $+10\text{ pts}$ per MTO completed, boosting *Infomorph Decryption Key* yield up to $+50\%$ or $+350$ bonus infomorphs) and **Anarchy** ($0..4$, passive $+5\text{ pts}/24\text{h}$, reduced by MTO completion, disrupting host Skyhook workforce by $0\%$ to $-40\%$).
5. **Exploration Hacking Minigame, Ghost Sites, Sleeper Caches, Chemical Labs & Medical Boosters (`source: Hacking.md, Ghost_Sites.md, Sleeper_Cache.md, Medical_boosters.md`):**
   - **Hacking Node-Graph (`hackingMgr`, `Hacking.md`):** Hexagonal board where System Core is placed $\ge 8$ hops from start (**Rule of 8**), non-border white nodes adjacent to the Core cannot show $1..5$ distance (**Rule of 6**); tracks Virus **Coherence** (`Archaeology`/`Hacking` $+10/\text{lvl}$) and **Strength**; 5 Defensive Subsystems (*System Core*, *Firewall*, *Antivirus*, *Restoration Node*, *Virus Suppressor*), 4 Utility Subsystems (*Self-Repair*, *Kernel Rot*, *Polymorphic Shield*, *Secondary Vector*), and Data Caches.
   - **Ghost Sites / Covert Research Facilities (`Ghost_Sites.md`):** *Lesser* (HS), *Standard* (LS), *Improved* (NS), *Superior* (WH). Hidden $30\text{s}–180\text{s}$ timer starts upon decloak; failing a hack detonates the container for heavy raw damage; when the timer expires, a pirate response squad warps in, scrams the ship, and blows up all cans (drops `Ascendancy` implant BPCs & Shattered Villard Wheels).
   - **Sleeper Caches (`Limited`, `Standard`, `Superior`, `Sleeper_Cache.md`):** Multi-pocket hazard puzzles (toxic gas clouds, plasma chambers, minefields, pressure plates, rifts, remote repair/hacking triggers, sentry turrets) that despawn from probe scanners once the first relic can is hacked; only source of **Polarized Weapon BPCs**.
   - **Chemical Labs (`30` sites) & Medical Boosters (`Medical_boosters.md`):** Harvest Mykoserocin/Cytoserocin gas (`71` gas sites) and react with Chemical Lab BPCs into *Synth*, *Standard* ($20\%$ side-effect chance), *Improved* ($30\%$), and *Strong* ($40\%$) boosters across Slots 1–3 (*Blue Pill*, *Exile*, *Mindflood*, *X-Instinct*, *Drop*, *Frentix*, *Sooth Sayer*, *Crash*) + Slot 10 (*Cerebral Accelerators*), modified by `Biology` ($+20\%$ duration/lvl), `Neurotoxin Control` ($-5\%$ penalty potency/lvl), and `Neurotoxin Recovery` ($-5\%$ side-effect chance/lvl).
6. **Wormhole Space (`J-Space`, `Wormhole_space.md`) & Roaming Metaliminal Storms (`Metaliminal_Storms.md`):**
   - **Wormhole Classes C1–C6 & 6 Environmental Weather Effects (`dbuffcollections.static`):** Exact C1 $\rightarrow$ C6 multipliers ($\pm 15\%$ to $\pm 50\%$ penalties, $+30\%$ to $+100\%$ bonuses, $+60\%$ to $+200\%$ small weapon damage in Wolf-Rayet) for *Pulsar*, *Black Hole*, *Cataclysmic Variable*, *Magnetar*, *Red Giant*, and *Wolf-Rayet*, plus the 5 **Drifter Wormholes** (*Conflux* J110145, *Barbican* J164710, *Redoubt* J174618, *Sentinel* J200727, *Vidette* J055520), **Shattered Wormholes**, and **Thera**.
   - **Metaliminal Storms:** Roaming Nullsec weather (*Gamma*, *Electrical*, *Plasma*, *Exotic*) with a 1-jump Core and 2–3 jump Periphery applying system-wide buffs/debuffs and spawning bonus exploration sites.
7. **Zarzakh (`Zarzakh.md`), Pirate Insurgencies, Corruption/Suppression Stages 1–5 (`Corruption_and_Suppression.md`) & Ambition Modifier (`Ambition_modifier.md`):**
   - **Zarzakh & The Fulcrum:** Connected to *Alsavoinon*, *Turnur*, *H-PA29*, and *G-0Q86*; charges a mass-based **Gate Toll** ($10\text{k}–100\text{M ISK}$, waived for pirate militia); applies a **6-hour Emanation Lock** restricting exit to the same entry gate (unless exiting via the **Deathless Shipcaster** to the Insurgency FOB, Clone Jump, or Pod); disables Bookmarks, D-Scan, and Capital gate jumps; applies volumetric gravity hazard damage outside safe zones; hosts **The Fulcrum** ($+6\%$ ME / $+70\%$ TE on Angel/Guristas subcaps, $-90\%$ SCC surcharge for pirate militia).
   - **Corruption & Suppression Stages 1–5 (`insurgencySolarsystem`):**
     - *Corruption Stages 1–5:* $+7.5\%$ to $+37.5\%$ PvP loot drop bonus for pirates; Stage 2 stops Empire Faction Police in Highsec; Stage 3 spreads Insurgency to an adjacent system; Stage 4 grants $+2\text{ AU/s}$ warp speed to pirate militia; **Stage 5 (Highsec)** makes attacking ships/structures/drones **lawless (no Criminal timer and no CONCORD response except on pod kills)**; **Stage 5 (Lowsec)** disables Sentry Gun criminal response and unlocks Interdiction Bubbles, Bomb Launchers, and Defender Missiles in Lowsec.
     - *Suppression Stages 1–5:* $+5\%$ to $+25\%$ NPC bounty & sec status gains, $+5\%$ to $+20\%$ empire FW LP; Stage 3 reduces Corruption gain by $30\%$; Stage 4 grants $+10\%$ Stasis Webifier & Warp Scrambler range to empire militia; **Stage 5** deploys Counter-Insurgency Sentry Guns and blocks Corruption from reaching Stage 5.
     - *Ambition Modifier:* Consecutive Insurgency campaign wins increment the winning side's Ambition modifier ($+1$ per win), increasing the number of Corruption 5 / Suppression 5 systems required to win the next campaign above the baseline of $5$.
8. **Upwell Structures, Orbital Skyhook Raiding (`Orbital_Skyhook.md`), Metenox Moon Drills (`Metenox_Moon_Drill.md`), ESS (`Encounter_Surveillance_System.md`) & Cloaking/Nullifiers:**
   - **Orbital Skyhooks (`orbitalSkyhook`):** Harvest Planetary Power, Workforce, and Reagents (*Magmatic Gas* from Lava planets, *Superionic Ice* from Ice planets) split 50/50 between the *Secure Bay* and *Surplus Bay*. Every 3–4 days, a 2-hour raiding vulnerability window opens (Gaussian offset $\sigma = 3\text{h}$ around defender's chosen window). Damaging the Reagent Silo to $\le 10\%$ Shield allows an Omega Cruiser+ or Upwell Hauler to link for **10 minutes** (speed capped at $1,000\text{ m/s}$, cannot warp/MJD/cloak/tether, broadcasting system alerts at $0\text{m}$, 1 jump at $2\text{m}$, and 2 jumps at $5\text{m}$) to drop the Surplus Bay.
   - **Metenox Moon Drill (`autoMoonMiner`):** Automated moon harvesting consuming Fuel Blocks + Magmatic Gas ($40\%$ efficiency); no docking/tethering, no damage cap, and **no automatic Shield/Armor repair** (must be manually repaired to $100\%$ by Logistics ships before it can enter reinforcement again).
   - **Encounter Surveillance System (`essMgr`):** 100km decloak sphere at beacon, Cruiser–Battleship gate (excluding Marauders), 75km deadspace interdiction bubble disabling Warp/MWD/MJD/Cyno/Filaments, 500km Cloak jam, 3-hour Main Bank auto-payout + 5-minute link robbery, and Reserve Bank unlocked via SCC Keys.
   - **Cloaking (`Cloaking.md`) & Interdiction Nullifiers (`Interdiction_Nullifier.md`):** $2,000\text{ m}$ decloak proximity, targeting delay, cloak stabilization timer, and active Interdiction Nullifier module (activatable while cloaked, negates launch & drag/catch bubbles, disabled if Burst Jammer or Higgs Anchor fitted).

---


---

### 3.11 Complete V24.01 Modern Feature Parity Specification

Our top-down audit of all `217` cataloged Remote Services (`193` V24.01 active + `24` Crucible legacy), `1,311` Notifications, `135` Quasar Protobuf Domains, `213` StaticData Archives, and `4,102` Wiki Articles covers **10 additional modern V24.01 feature systems**:

#### 1. *Catalyst* Expansion (`Nov 2025`, Verified in V24.01 `code.ccp`): Phased Asteroid Fields, Mobile Phase Anchor & Prismaticite (`Phased_Asteroid_Field.md`)
- **Cosmic Signature & Moving Hyperspace Fracture:**
  - Spawns as a Cosmic Signature across Nullsec (`-0.01` to `-1.0`). Contains normal asteroids (**Griemeer**, **Hezorime**, **Ueganite**, **Mercoxit**) at the warp-in plus a cluster of **Phased Asteroids (Prismaticite)** and a slowly moving **Hyperspace Fracture** (an environmental cloud that drifts toward the center of the site, granting a **Mining Crit Chance Bonus** while penalizing Shield Booster and Logistics duration inside its radius; jumping the entry Acceleration Gate while cloaked is blocked).
- **Phased-Out vs. Phased-In State & Mobile Phase Anchor (`typeID` from *Fractured Legacy* Epic Arc `EPICARC_ORE = 128` / ORE LP):**
  - By default, **Prismaticite** asteroids are **Phased-Out** ($10\%$ mining yield, effectively $90\%$ residue waste).
  - Deploying and linking a **Mobile Phase Anchor** within $20\text{ km}$ of the Phased Asteroids requires an Industrial Command Ship (**Porpoise** provides $35$ energy, **Orca** $50$ energy, **Rorqual** $100$ energy) firing a **Command Burst** to power the Anchor to **$100$ Energy** (3 Porpoises, 2 Orcas, or 1 Rorqual).
  - Once powered to $100$, all Prismaticite rocks within $20\text{ km}$ transition to **Phased-In** ($100\%$ normal mining yield) and spawn **Rogue Drone defense waves** every few minutes.
- **Prismaticite Erratic Reprocessing vs. Unrefined Mineral Reactions (`reprocessingSvc`, `ramProxy`):**
  - Standard station reprocessing of **Prismaticite** (`Erratic Reprocessing`) randomly converts the batch into **exactly 1 of the 8 minerals** (*Tritanium*, *Pyerite*, *Mexallon*, *Isogen*, *Nocxium*, *Zydrine*, *Megacyte*, or *Morphite*) weighted by equal ISK value.
  - Alternatively, running an **Unrefined Mineral Reaction Formula** in a **Standup Composite Reactor** allows industrialists to deterministically choose which specific mineral Prismaticite refines into.

#### 2. Complete Personal Deployables, Mobile Structures & CONCORD Rogue Analysis Beacon (`CRAB`) (`source: Mobile_structures.md, spacecomponentsbytype.fsdbinary`)
All deployables are modeled in `crates/eve-server/src/space_components/` using exact parameters from `spacecomponentsbytype.fsdbinary`:
| Deployable Structure | Variants | Anchor Time & Restrictions | Core Server Mechanics |
|---|---|---|---|
| **Mobile Depot** | Standard, `'Wetu'`, `'Yurt'` | $60\text{s}$, $50\text{ km}$ from gates/stations | Provides $3,000\text{–}4,000\text{ m}^3$ cargo + Ship Fitting Service within $3,000\text{ m}$. Enters **48-hour Reinforcement** at $25\%$ shield (`reinforce` component). Lasts 30 days. |
| **Mobile Tractor Unit (`MTU`)** | Standard, `'Packrat'`, `'Magpie'` | $10\text{s}$, $7\text{ km}$ between MTUs | Pulls wrecks/cans within $125\text{–}200\text{ km}$ at $1,000\text{–}1,500\text{ m/s}$ (`autoTractorBeam`) and loots them into its $27,500\text{–}50,000\text{ m}^3$ bay (`autoLooter`). |
| **Mobile Cynosural Inhibitor** | Standard | $120\text{s}$, $200\text{ km}$ from structures | Blocks regular & Industrial Cynosural Fields within $100\text{ km}$ (Covert Cynos unaffected) for 1 hour. |
| **Mobile Scan Inhibitor** | Standard | $60\text{s}$, $30\text{ km}$ radius | Hides ships and deployables inside $30\text{ km}$ from **Directional Scanner (`D-Scan`)** and **Combat Scanner Probes** (the Inhibitor itself remains probable) for 1 hour. |
| **Mobile Observatory** | Standard | **$10\text{ min}$ (`600s`)**, Lowsec/Nullsec (`Mobile_structures.md:101`) | Lasts up to **2 hours (`7,200s`)** and pings system-wide every **10 minutes**. Each ping rolls a **$40\%$ chance to decloak** any cloaked ship in the solar system whose **15-minute Cloak Stabilization Timer** has expired. |
| **Mobile Micro Jump Unit** | Standard | $60\text{s}$, $50\text{ km}$ from gates | Any ship within $2,500\text{ m}$ can activate it (`microJumpDriver` component): spools for $12\text{s}$ and jumps the ship $100\text{ km}$ along its heading vector (blocked by Warp Scramblers). |
| **Mobile Warp Disruptor** | Small / Med / Large T1, T2, `'Syndicate'` | $30\text{–}120\text{s}$, **Nullsec / W-Space only** | Generates a $10\text{–}39.5\text{ km}$ Warp Disruption Bubble (`warpDisruption` component). Captures sub-warp ships inside and **drags/catches** ships whose warp vector passes within $500\text{ km}$ of the bubble center. Decays after 2–14 days. |
| **Mobile Cynosural Beacon** | Standard & Covert (`Black Ops`) | $60\text{–}120\text{s}$, $\le 0.4$ Sec | Broadcasts a fleet-accessible Cynosural jump beacon for 1 hour without tying down a Recon/Industrial ship. |
| **CONCORD Rogue Analysis Beacon (`CRAB`)** | Standard | $60\text{s}$, $\le 0.4$ Sec, Capital/Supercap only | Capital ship links (`linkWithShip`) for 4 minutes ($+10\%$ all resists, blocks Jump Drive/Tether), then defends the beacon for **10 minutes** against Rogue Drone waves (Frigates up to Dreadnoughts/Carriers) before a 5-minute extraction timer drops **Rogue Drone Infested Salvage**. Consumes character **Complex Encryption Qubits (`CEQ`)** (`characterEnergyMgr`: max $5,000$ CEQ, regenerates $3\text{ CEQ/min}$, costs $2,160\text{ CEQ}$ per link) and increases solar system **Signal Interference** (`solarsystemInterferenceMgr`, max $12$ CRAB runs per system per 23h). |

#### 3. Travel, Pochven & Arena Filaments (`Filament.md`, `randomJumpMgr`, `pvpFilamentMgr`)
- **Spool-Up & Fleet Trace Mechanics (`randomJumpMgr.ActivateRandomJumpFilament`):**
  - Requires player to be in a Fleet, at least $150\text{ km}$ from any celestial/stargate/station/structure, with **no Weapons Timer** (and no PvP Logoff Timer for Pochven/Needlejack).
  - Activating a filament spawns a **Filament Trace** for $15\text{s}–210\text{s}$. When activated again (or when all up to `5`, `15`, or `25` fleetmates are within $30\text{ km}$ of the trace), all eligible fleet ships are instantaneously jumped across solar systems (`OnSessionChanged` + `AddBalls2`).
- **Complete Filament Catalog:**
  1. **Noise (`-5`/`-15`/`-25` ‘Needlejack’):** Jumps fleet to a **random quiet Nullsec system** weighted toward low recent player activity.
  2. **Signal (`-5`/`-15`/`-25` ‘Needlejack’):** Jumps fleet to a **random active Nullsec system** weighted toward high recent player count / ratting / PvP kills.
  3. **Inbound Pochven (`Border` / `Internal` / `Cladistic` *Krai Perun*, *Veles*, *Svarog*):** Jumps fleet into a Pochven system of the matching Krai/tier.
  4. **Outbound Pochven (`Glorification ‘Devana’` & `Proximity ‘Extraction’`):** Jumps fleet out of Pochven to a random Triglavian Minor Victory system (`Devana`) or a K-space system within light-year proximity (`Extraction`).
  5. **Ice Storm (`‘Rime’`):** Jumps fleet to a solar system currently inside a **Metaliminal Ice Storm**.
  6. **Abyssal Proving Grounds (`pvpFilamentMgr`):** Matchmakes solo/duo/fleet ships into an instanced **Proving Grounds PvP Arena** pocket with shrinking boundary sphere, anti-stalemate resistance reduction timer, and leaderboard tracking (`leaderboard/`).

#### 4. Titan Doomsday Superweapons & Phenomena Generators (`superWeaponMgr`)
- **Single-Target Doomsdays (`Aurora Ominae` EM, `Oblivion` Kinetic, `Judgement` Thermal, `Gjallarhorn` Explosive):**
  - Consumes $1,000\text{ Strontium Clathrates}$; immobilizes Titan for $30\text{s}$, blocks Tether/Warp/Jump/Dock for **5 minutes**, and deals $1,000,000$ base damage over a $6\text{s}$ beam to a single Capital/Supercapital target.
- **Area-of-Effect (`AoE`) Lance, Bosonic Field Generator & Reaper Doomsdays (`superWeaponMgr.ActivateSlashModule`):**
  - **Lance (`Focused`):** Fires a $100\text{ km} \times 2.5\text{ km}$ cylindrical beam along a user-aimed vector (`startPoint` $\rightarrow$ `endPoint`) after a $15\text{s}$ spool.
  - **Bosonic Field Generator (`BFG`):** Sweeps a $60^\circ$ cone ($50\text{ km}$ range) over $20\text{s}$ between two spatial vectors.
  - **Reaper (`Gravitational`):** Sweeps a close-range arc cleavage beam.
- **Gravitational Transportation Field Oscillator (`GTFO`):** Teleports all ships within a targeted $15\text{ km}$ sphere to a random point $100\text{ km}$ away in the solar system.
- **Phenomena Generators:** Titan-mounted system-wide/grid-wide environmental fleet buffs (`dbuffcollections.static`, e.g., $+30\%$ Armor HP / $-15\%$ Signature Radius).

#### 5. Upwell Structure Power States, Quantum Cores & Asset Safety (`source: Quantum_Cores.md, Asset_safety.md, structureAssetSafety`)
- **3 Upwell Power States:**
  1. **Full Power:** At least 1 Service Module online and consuming Fuel Blocks; full Shield $\rightarrow$ Armor $\rightarrow$ Hull reinforcement cycles.
  2. **Low Power:** 0 Service Modules online (or out of fuel); skips Shield timer (Armor $\rightarrow$ Hull only, with reduced EHP).
  3. **Abandoned State (7 Days continuously in Low Power):** **Zero reinforcement timers** (Shield, Armor, and Hull can be destroyed in a single attack session), **Tethering disabled**, and **Asset Safety is disabled — 100% of all player ships and items inside the structure drop as wreck loot containers in space.**
- **Mandatory Quantum Core Mechanics:**
  - Newly anchored Upwell structures remain in the initial Hull Vulnerability state until the matching **Upwell Quantum Core** is installed into the **Core Room** (`structureSettings`).
  - When an Upwell structure is destroyed or unanchored, its **Quantum Core drops $100\%$ of the time**.
- **Asset Safety Wrap & Delivery (`structureAssetSafety`, `inventoryAssetHoldingMgr`):**
  - Triggered manually by a player or automatically when a Full/Low Power structure in K-space is destroyed (Wormhole structures never have Asset Safety):
    - **Same-System Delivery (Available after 5 Days):** Player can deliver the `Asset Safety Wrap` to any station or structure in the **same solar system** for a **$0.5\%$ fee** of estimated item value (`Asset_safety.md:54`).
    - **Automatic NPC Station Delivery (After 20 Days):** Automatically moved to the nearest Lowsec/Highsec NPC station with a **$15\%$ fee** required to unlock the wrap (`Asset_safety.md:49`).

#### 6. Killmails, Hull Killmarks & 30-Day Kill Rights (`source: warStatisticMgr, Killmarks.md, Kill_Rights.md:20-23, killRightMgr, crimewatch`)
- **Cryptographic Killmails (`warStatisticMgr.GetKillMail`, `GetKillMailIDandHash`):**
  - Every ship, capsule, or structure destruction generates a durable `killmails` record containing victim character/corp/alliance, ship `typeID`, solar system, coordinates, total damage taken, list of all attackers (`damageDone`, `finalBlow`, `weaponTypeID`, `shipTypeID`), and every destroyed vs. dropped item by `flagID`, plus a SHA-1 `killHash` for external verification.
- **Hull Killmarks (`Killmarks.md`, `shipKillCounter.GetShipKillCount`):**
  - Destroying a capsuleer-piloted Combat Ship in K-space/W-space increments the final-blow ship's persistent `killCount` (up to `999`), rendered on the 3D hull via `1-kill`, `10-kill`, and `100-kill` Empire/Faction decals. Repackaging or destroying the ship resets its `killCount` to `0`.
- **30-Day Kill Rights (`Kill_Rights.md:20-23`, `killRightMgr`, `crimewatch`):**
  - Awarded **only on criminal aggression**: **(a) unlawful aggression against a capsuleer ship, capsule, or structure in Highsec**, or **(b) unlawful aggression against a Capsule (pod) in Lowsec** (`OnKillRightCreated`). Ordinary unlawful ship aggression in Lowsec triggers only a Suspect flag and **never** grants a Kill Right.
  - The victim can keep the Kill Right private or make it available to **All Players**, their **Corporation**, or their **Alliance** for a custom **Activation Fee (ISK)** (`SellKillRight` / `CancelSellKillRight`).
  - When any eligible player pays the Activation Fee while on grid with the offender (`ActivateKillRight`), the offender immediately receives a **15-minute Suspect Flag** (`OnKillRightActivated`), allowing anyone to legally engage them in Highsec without CONCORD intervention (`Kill_Rights.md:40`).

#### 7. Corporation Projects & Freelance Projects (`freelanceprojects/`, `corporation/goal/`, `eve_public.freelance`, `eve_public.corporationgoal`)
- **12 Tracked Contribution Archetypes (`corporation/goal`, `freelanceprojects/common/`):**
  - *Damage Capsuleers*, *Destroy Capsuleer Ships*, *Destroy NPCs*, *Capture FW Complexes*, *Defend FW Complexes*, *Deliver Items to Hangar*, *Mine Ore*, *Manufacture Items*, *Salvage Wrecks*, *Scan Signatures*, *Remote Repair Shield*, and *Remote Repair Armor*.
- **Automatic ISK Escrow & Per-Contribution Payouts:**
  - Corporations deposit an ISK budget into project escrow, set `rewardPerContribution`, `maxContributionPerParticipant`, `targetProgress`, **Freelance Eligibility** (Public vs. Corp-only, max character age for New Player projects, Career Path tags), and Solar System / Station filters.
  - As players perform matching actions anywhere in the specified systems, `EvoMu` automatically increments project progress (`OnCorporationGoalProgressed`) and disburses ISK from escrow.

#### 8. SKINR Custom Ship Studio, Paragon Hub & EverMarks (`cosmetics/`, `shipcosmetics/`, `shipCosmeticsMgr`, `shipCosmeticsMgr`)
- **Paragon LP, EverMarks & Structure Paintworks (`cosmetics/common/structures/`):**
  - Players earn **EverMarks** from Paragon **IRIS** daily ship-turn-in missions and The Agency, and spend them at Paragon stations on **3D Holographic Corporation & Alliance Ship Emblems** (`shipCosmeticsMgr`) and **Upwell Structure Paintworks** (`shipCosmeticsMgr`, duration-based 30/90/180-day structure liveries).
- **SKINR Ship Custom Skin Studio (`shipcosmetics/`, `eve_public.cosmetic.ship`):**
  - Players combine **Basic/Metallic/Matte Nanocoatings** across 4 hull material slots + **Patterns** to sequence custom ship skins (**Sequencing Jobs** consuming PLEX or Sequencing Accelerators, modified by `Batch Sequencing` / `Sequencing Time` skills) and list/trade them in the **Paragon Hub**.

#### 9. Faction Warfare Direct Enlistment & Diplomatic War HQ / Peace Treaties (`fwCharacterEnlistmentMgr`, `warsInfoMgr`, `mutualWarInviteMgr`, `peaceTreatyMgr`)
- **Direct Enlistment (`fwCharacterEnlistmentMgr`):** Allows individual characters to enlist directly in an Empire or Pirate Militia (`CreateMyDirectEnlistment`, `RemoveMyDirectEnlistment` with cooldown) without their player corporation leaving its Alliance.
- **War HQ, Allies, Mutual Wars & Surrender/Peace Treaties (`warsInfoMgr`, `mutualWarInviteMgr`, `peaceTreatyMgr`):**
  - Aggressive Highsec/Lowsec wars require designating an Upwell Structure in Highsec as the **War HQ** (`100M ISK/week` CONCORD War Bill). If the War HQ is destroyed, the war ends after a 24-hour cooldown and both sides enter a **14-day Forced Peace Treaty** (`peaceTreatyMgr`). Defenders can hire **Mercenary Allies** (`NegotiateAllyContract`).

#### 10. Ship Progression ("Boarding Moments"), Expert Systems, Project Discovery & Seasonal Events (`shipprogression/`, `expertSystems/`, `projectdiscovery/`, `seasons/`)
- **Ship Progression & Cinematic Boarding Moments (`shipprogression/`, `eve_public.ship`):** Tracks first-time hull boarding milestones (`GetUnlockedShipIds`) and triggers the V24.01 cinematic hangar camera sequence when boarding a newly unlocked hull class or Titan/Capital for the first time.
- **Expert Systems (`expertSystemMgr`):** Temporary (3-to-7 day) virtual skill-bundle licenses (`ConsumeExpertSystem`, `GetMyExpertSystems`) that grant effective skill levels without modifying character SP or requiring skillbooks.
- **Project Discovery (`ProjectDiscovery`):** Citizen-science flow-cytometry polygon gating minigame awarding XP levels, Marshal/Pacifier/Enforcer BPCs, and skins.
- **Seasonal Events, Community Tracks & Daily Login Campaigns (`seasonManager`, `seasonalLoginCampaignManager`, `communityTrackMgr`):** Supports holiday events (*Crimson Harvest*, *Winter Nexus*, *Capsuleer Day*, *Federation Day*, *Liberation Day*), event point tracks, and login rewards.

---

