# EvoMu Deep-Dive Spec 01: Character Creation, Skills, Ships, Fitting & 18-Domain Overview (`§3.1`–`§3.5`)

> Part of the [`PLAN.md`](../../PLAN.md) Master Specification.

---

### 3.1 Races, Bloodlines, Ancestries, Schools & Character Creation
- **Authoritative Sources:** `source: res:/staticdata/races.fsdbinary, bloodlines.fsdbinary, ancestries.fsdbinary, schools.fsdbinary, schoolmap.fsdbinary, empireselection.static, paperdolls.fsdbinary, eve/common/lib/appConst.pyj, chrSchools.sql.gz`.

1. **The 4 Playable Empires, 12 Bloodlines & 12 Starter Schools (+ Jove/Triglavian/CONCORD NPCs):**
   - **Caldari State (`raceID = 1`, faction `500001`):**
     - Bloodlines: *Deteis* (`1`), *Civire* (`2`), *Achura* (`11`).
     - Schools (starter NPC corporations & headquarters stations, verified in `chrSchools.sql.gz` & `appConst.pyj`):
       - *State War Academy* (`schoolID = 17`, `corpID = 1000167`, station `60015001`),
       - *Science and Trade Institute* (`schoolID = 18`, `corpID = 1000045`, station `60004420`),
       - *School of Applied Knowledge* (`schoolID = 19`, `corpID = 1000044`, station `60004462`).
     - Starter Corvette: **Ibis** (`typeID = 601`).
   - **Minmatar Republic (`raceID = 2`, faction `500002`):**
     - Bloodlines: *Sebiestor* (`3`), *Brutor* (`4`), *Vherokior* (`14`).
     - Schools:
       - *Republic Military School* (`schoolID = 14`, `corpID = 1000170`, station `60015021`),
       - *Republic University* (`schoolID = 15`, `corpID = 1000171`, station `60015016`),
       - *Pator Tech School* (`schoolID = 16`, `corpID = 1000172`, station `60015027`).
     - Starter Corvette: **Reaper** (`typeID = 588`).
   - **Amarr Empire (`raceID = 4`, faction `500003`):**
     - Bloodlines: *Amarr* (`5`), *Ni-Kunni* (`6`), *Khanid* (`13`).
     - Schools:
       - *Imperial Academy* (`schoolID = 11`, `corpID = 1000166`, station `60015006`),
       - *Hedion University* (`schoolID = 12`, `corpID = 1000165`, station `60015005`),
       - *Royal Amarr Institute* (`schoolID = 13`, `corpID = 1000077`, station `60007579`).
     - Starter Corvette: **Impairor** (`typeID = 596`).
   - **Gallente Federation (`raceID = 8`, faction `500004`):**
     - Bloodlines: *Gallente* (`7`), *Intaki* (`8`), *Jin-Mei* (`12`).
     - Schools:
       - *Federal Navy Academy* (`schoolID = 20`, `corpID = 1000168`, station `60015036`),
       - *University of Caille* (`schoolID = 21`, `corpID = 1000115`, station `60011566`),
       - *Center for Advanced Studies* (`schoolID = 22`, `corpID = 1000169`, station `60015042`).
     - Starter Corvette: **Velator** (`typeID = 606`).
2. **Character Creation Transaction (`charUnboundMgr.CreateCharacterWithDoll`):**
   - Validates character name uniqueness and regex rules (`ValidateNameEx`).
   - Resolves `raceID`, `bloodlineID`, `ancestryID`, and `schoolID` (`schoolmap.fsdbinary`, `schools.fsdbinary`) to determine:
     - Starting `corporationID` (the chosen School NPC corporation),
     - Starting `stationID` & `solarSystemID` (the School's starter station or Career Hub),
     - Base neural attributes: `charisma` (`164`), `intelligence` (`165`), `memory` (`166`), `perception` (`167`), `willpower` (`168`) — initialized to the standard balanced baseline (`20` Perception, `20` Willpower, `20` Intelligence, `20` Memory, `19` Charisma = `99` base points before neural remap).
   - Seeds the **racial/school starter skill set** dynamically from the SDE starter skill tables (`sklCareerSkills` / `clonegrades.static` initial levels across Gunnery, Missiles, Spaceship Command, Engineering, Navigation, Electronic Systems, Armor, Shields, Drones, and the racial Frigate skill).
   - Spawns the character's **Capsule (`typeID = 670`)** + racial **Corvette** (fitted with 1 Civilian Miner, 1 Civilian weapon, and 1 unit of Tritanium in cargo) inside the starter station hangar, sets `active_ship_id`, and stores the 3D `paperDoll` sculpt (`paperDollServer`).

---

### 3.2 Skills, Neural Attributes, Clone Grades (Alpha/Omega), Certificates, Mastery & Skill Plans
- **Authoritative Sources:** `source: res:/staticdata/clonegrades.static, requiredskillsfortypes.fsdbinary, certificates.static, skillplans.fsdbinary, eve/client/script/ui/shared/neocom/skillConst.pyj, Skill_training.md`.

1. **Skill Point (SP) Curve & Rank Formula:**
   - Every skill (`categoryID = 16`) has a Dogma **Skill Time Constant / Rank** ($R \in \{1..16\}$, attribute `skillTimeConstant = 275`) and two training attributes: `primaryAttribute` (`180`) and `secondaryAttribute` (`181`).
   - **Exact SP required for Level $L \in \{1..5\}$ (`source: Skill_training.md`):**
     $$\text{SP}(L, R) = \left\lceil 250 \times R \times \left(\sqrt{32}\right)^{L - 1} \right\rceil$$
     - Note that the ceiling $\lceil \cdot \rceil$ is applied *after* multiplying $250 \times 2^{2.5(L-1)}$ by Rank $R$. For Rank $R = 1$, $\text{SP}(L, 1) \in \{250,\ 1,415,\ 8,000,\ 45,255,\ 256,000\}$; for $R > 1$, $\text{SP}(2, 2) = \lceil 500 \times \sqrt{32} \rceil = 2,829$ (not $2 \times 1,415 = 2,830$).
2. **Training Rate Formula (`SP/minute`):**
   $$\text{Rate}_{\text{SP/min}} = \left(\text{Attr}_{\text{primary}} + 0.5 \times \text{Attr}_{\text{secondary}}\right) \times M_{\text{clone}}$$
   - For **Omega Clones**, $M_{\text{clone}} = 1.0$ (e.g., with primary $27$, secondary $21$, $\text{Rate} = 37.5\text{ SP/min} = 2,250\text{ SP/hr}$).
   - For **Alpha Clones**, $M_{\text{clone}} = 0.5$ ($0.5\times$ training speed, free training paused above $5,000,000\text{ SP}$, max $20.4\text{M SP}$ with injectors).
   - **Cerebral Accelerators (Boosters)** & **Learning Implants (`slot 1..5`)** add $+1$ to $+12$ to `Attr_primary` and `Attr_secondary` via Dogma modifiers.
3. **Alpha vs. Omega Clone Grades (`clonegrades.static` & `subscriptionMgr`):**
   - `clonegrades.static` defines the **SDE-defined per-race allowed skills (`res:/staticdata/clonegrades.static`, `49,152 B` SQLite3; `~204` skills per `~/ai/eve-wiki/articles/Alpha_and_Omega_Clone.md`, capped at `CLONE_STATE_ALPHA_MAX_TRAINING_SP = 5,000,000` SP in `clonegrade/const.pyj`) and max level caps (`level 1..5`)** for each race (`raceID` `1`, `2`, `4`, `8`).
   - When a character is in Alpha state (`cloneGrade = 0`), their *effective* skill level used by `eve-dogma` (`effectiveSkillLevel`) is $\min(\text{trainedLevel}, \text{alphaMaxLevel})$, while `trainedLevel` and `skillPoints` are preserved in `eve-db` if they upgrade to Omega (`cloneGrade = 1`).
4. **Skill Queue, Free SP, Injectors & Neural Remaps (`skillMgr2` / `skillHandler` + Quasar `eve_public.skill`):**
   - **Skill Queue (`skill_queue` table):** Ordered list of `(skill_type_id, target_level, start_sp, end_sp, start_time, end_time)`. When the server processes a request or a background timer fires, any completed queue entries emit `OnSkillTrained` / `OnGodmaSkillTrained` and dynamically recompute the active ship's Dogma attributes.
   - **Unallocated / Free SP (`ApplyFreeSkillPoints`):** Instant SP pool granted by Daily Goals, Login Campaigns, or **Skill Injectors** (diminishing returns based on total character SP: $500\text{k SP}$ under $5\text{M SP}$; $400\text{k SP}$ for $5\text{M}–50\text{M}$; $300\text{k SP}$ for $50\text{M}–80\text{M}$; $150\text{k SP}$ above $80\text{M SP}$).
   - **Skill Extractors:** Requires $\ge 5.5\text{M SP}$; extracts $500,000\text{ SP}$ from selected skills (cannot extract prerequisites of other trained skills) and creates a `Large Skill Injector` item in the hangar.
   - **Certificates & Ship Masteries (`certificates.static` + `certificateMgr`):** Every ship has Mastery Levels I–V composed of Certificates, which are in turn composed of 5 skill levels.
   - **Skill Plans (`skillplans.fsdbinary` + Quasar `eve_public.skill.plan`):** Certified career skill plans and personal/corp shared skill plans.

---

### 3.3 Ships, Hull Classes, Tech I/II/III, Tactical Modes, Subsystems, Capitals, Mutaplasmids & SKINR
- **Authoritative Sources:** `source: res:/staticdata/types.fsdbinary, typedogma.fsdbinary, infobubbles.static, dynamicitemattributes.fsdbinary, skins.static, wrecks.static, Weapons.md`.

1. **Ship Taxonomy & Hull Role/Skill Bonuses (`infobubbles.static` & `typedogma.fsdbinary`):**
   - Every ship hull (`categoryID = 6`) has:
     - **Base Attributes:** Structure/Armor/Shield HP & Resonances, Capacitor & Recharge Time, CPU, Powergrid, Calibration, High/Med/Low/Rig/Subsystem slots, Turret/Launcher hardpoints, Drone Bay capacity (`droneCapacity`) & Bandwidth (`droneBandwidth`), Cargo capacity + specialized bays (Fleet Hangar, Ship Maintenance Bay, Fuel Bay, Ore/Gas/Ice Hold, Ammo Hold, Corpse Bay, Frigate Escape Bay).
     - **Per-Skill Level Bonuses:** Applied via Dogma `LocationRequiredSkillModifier` / `OwnerRequiredSkillModifier` expressions (e.g., `+5% Small Projectile Turret rate of fire per level of Minmatar Frigate`).
     - **Role Bonuses:** Applied via ship-intrinsic Dogma effects (`shipBonusRole*`).
2. **Special Ship Mechanics:**
   - **Tech III Tactical Destroyers (`shipmode/` — Confessor, Svipul, Jackdaw, Hecate):**
     - Switch between **Defense Mode**, **Propulsion Mode**, and **Sharpshooter Mode** (`ship.SetTacticalMode`) with a 10-second cooldown (`modeSwitchDelay`), swapping the hidden tactical mode item in `flagHiddenModifers` to apply $+33.3\%$ resistance, $+66.6\%$ speed/agility, or $+66.6\%$ range/sensor bonuses.
   - **Tech III Strategic Cruisers (Tengu, Loki, Legion, Proteus):**
     - Require **4 fitted Subsystems** (`flagSubSystemSlot0..3`: Core, Defensive, Offensive, Propulsion) which dynamically determine the ship's High/Med/Low slot counts, hardpoints, 3D visual model (`sofDnaLibrary`), and skill bonuses. Can be refitted in space using a Mobile Depot.
   - **Bastion, Siege, Triage & Industrial Core Modules (`typedogma.fsdbinary` attributes `consumptionType = 713`, `consumptionQuantity = 714`):**
     - Lock the ship in place (`speed_fraction = 0`, cannot warp/dock/jump, immune to remote assistance or EWAR depending on module) for the $60\text{s}–300\text{s}$ cycle duration:
       - **Bastion Module** (Marauders): consumes **no fuel** (`0` charges).
       - **Siege Module** (Dreadnoughts) & **Triage Module** (Force Auxiliaries): consume **Strontium Clathrates** (`typeID = 16275`).
       - **Industrial Core** (Rorqual): consumes **Heavy Water** (`typeID = 16272`).
   - **Capital, Supercapital & Titan Mechanics:**
     - **Jump Drives & Jump Fatigue (`jumpFatigue/`):** Consumes **Racial Isotopes** per light-year jumped (while Cynosural Fields and Ansiblex Jump Bridges consume **Liquid Ozone**, `typeID = 16273`); tracks `jumpFatigueMultiplier` (Blue timer, max 5 hours) and `jumpLastUpdated` (Orange reactivation timer, max 30 minutes).
     - **Ship Maintenance Bay & Fleet Hangar:** Allows storing assembled subcapital ships and allowing fleetmates to refit or board ships in space.
     - **Frigate Escape Bay:** Battleships, Marauders, Black Ops, and Capitals can store an assembled Frigate that the pilot automatically boards when the parent ship is destroyed.
     - **Clone Vat Bay (`jumpCloneSvc`):** Titans and Rorquals can host jump clones for fleet/corp members.
     - **Doomsdays & Phenomenon Generators (`superWeaponMgr`):** Targeted single-target Doomsdays, Lance/Reaper AoE beams (using 3D ray/cylinder intersection in `Ballpark`), Bosonic Field Generators, and System-Wide Phenomena (`dbuffcollections.static`).
   - **Triglavian Entropic Disintegrators (Damavik, Vedmak, Drekavac, Leshak, Zirnitra):**
     - Spool-up weapons whose damage multiplier increases by $+5\%$ (or more, governed by `damageMultiplierBonusPerCycle` in `typedogma.fsdbinary`) per consecutive cycle on the same target up to `damageMultiplierBonusMax`.
   - **EDENCOM Vorton Projectors (Skybreaker, Stormbringer, Thunderchild, `source: Weapons.md:144, typedogma.fsdbinary`):**
     - Chain-lightning weapons that hit the primary target and arc to **up to 9 additional targets within $10\text{ km}$** (up to 10 targets total, governed by `maxChainTargets` in `typedogma.fsdbinary`).
   - **Cenotaph / Deathless Damage-Over-Time (`dotWeapons/`, `Breacher_Pods.md`):**
     - Breacher Pod launchers applying hull/armor/shield raw-HP percentage-capped DoT effects over time.
   - **Abyssal Mutaplasmids (`dynamicItemService` + `dynamicitemattributes.fsdbinary`):**
     - Consumes a base module/drone + a Gravid/Unstable/Decayed Mutaplasmid to create a unique `dynamicItem` whose Dogma attributes (`damageModifier`, `cpu`, `power`, `duration`, `maxRange`) are rolled within `[min, max]` multiplier bounds and persisted per `itemID` in `eve-db`.
   - **Ship Cosmetics, SKINs & Paragon SKINR (`skins.static`, `ship_skin_design_components.fsdbinary`, `shipCosmeticsMgr`, Quasar `eve_public.cosmetic`):**
     - Tracks licensed SKINs (`skinlicenses.static`), corporation/alliance emblems (`shipCosmeticsMgr`), and custom SKINR sequenced designs (`sequencing/`).

---

### 3.4 Complete 18-Domain Server Architecture & Implementation Roadmap

| Domain | Representative Services (`machoNet` & Quasar) | Key Static Data Sources | Core Server Logic & State Machines |
|---|---|---|---|
| **1. Wire Protocol & Dual-Bus Transport** | `machoNet`, `publicGatewaySvc` | `EVEMarshalStringTable`, `eveProto/` | `eveMarshal` (`0x01`–`0x2C`), `PyPackedRow` RLE, `MachoPacket`, `SessionChangeNotification`, `evomu_bridge.py` + `PublicGatewayShim` |
| **2. Auth, Bootstrap & Caching** | `authentication`, `machoNet`, `objectCaching`, `config`, `alert`, `slash` | `globaltypes.fsdbinary` | `GetInitVals`, `objectCaching.GetCachableObject` (`CachedMethodCallResult`), `config.GetMultiOwnersEx`/`GetMultiLocationsEx`, `/slash` GM commands |
| **3. Races, Character Creation & PaperDoll** | `charUnboundMgr`, `charMgr`, `paperDollServer`, `onlineStatus` | `races`, `bloodlines`, `ancestries`, `schools`, `schoolmap`, `empireselection`, `paperdolls` | Name validation, race/bloodline/school resolution (`chrSchools.sql.gz`), starter skill & Corvette/Capsule seeding, 3D portrait/sculpt storage |
| **4. Skills, Clones, Masteries & Skill Plans** | `skillMgr2`, `skillHandler`, `subscriptionMgr`, `certificateMgr`, `jumpCloneSvc`, Quasar `skill` | `clonegrades.static`, `requiredskillsfortypes`, `certificates.static`, `skillplans` | Rank SP curve ($\lceil 250 \cdot R \cdot \sqrt{32}^{L-1}\rceil$), Alpha/Omega caps, real-time skill queue tick, Free SP, Injectors/Extractors, Neural Remap, Jump Clones |
| **5. Ships, Fitting & Mutaplasmids** | `ship`, `charFittingMgr`, `dogmaIM`, `dynamicItemService`, `insuranceSvc`, `repairSvc` | `types`, `typedogma`, `infobubbles.static`, `dynamicitemattributes`, `skins.static` | Ship boarding/assembling/repackaging, saved fittings (`charFittingMgr`), T3 Tactical Modes & Subsystems, Mutaplasmid rolls, Ship Insurance |
| **6. Dogma Attribute & Effect VM** | `dogmaIM`, `dogma`, `dbuffClient` | `dogmaattributes`, `dogmaeffects`, `dogmaunits`, `dbuffcollections.static` | 6-stage stacking-penalized modifier pipeline, `dgmExpressions` bytecode VM (`75` opcodes), nonlinear capacitor/shield recharge, 3-rack module overheating/burnout |
| **7. Inventory & Asset Management** | `invbroker`, `structureAssetSafety`, `inventoryAssetHoldingMgr` | `categories`, `groups`, `types`, `contrabandtypes` | Piggybacked `MachoBindObject`, `GetInventoryFromId`, `List`, `Add`, `MultiAdd`, `StackAll`, `Split`, `TrashItems`, `OnItemChange` transitions |
| **8. Station, Upwell Structure & World Topology** | `stationSvc`, `station`, `structureDocking`, `structureGuests`, `structureSettings`, `map` | `systems.static`, `jumps.static`, `solarsystemcontent.static`, `stationoperations` | Station & Upwell Structure Dock/Undock, 2500m perimeter check, 30s undock invulnerability / Upwell Tether, Stargate Jumps + 60s Gate Cloak |
| **9. Destiny 1 Hz Kinematics & Grid Bubbles** | `beyonce`, `michelle` | `traveleffects.fsdbinary`, `Destiny.xmlp` | 1 Hz `SystemActor`, exponential sub-warp align, 4-phase warp + capacitor consumption, 250km–8000km grid bubbles, binary `AddBalls2` |
| **10. Space Components ECS (100 Components)** | `spacecomponents`, `beyonce` | `spacecomponentsbytype.fsdbinary` | Component lifecycle hooks (`deploy`, `activate`, `scoop`, `decay`, `autoTractorBeam`, `autoLooter`, `fitting`, `reinforce`, `cynoInhibitor`, `warpDisruption`, `essLink`, `orbitalSkyhook`, etc.) |
| **11. Combat, Weapons, Drones, Fighters & CrimeWatch** | `dogmaIM`, `entity`, `fighterMgr`, `crimewatch`, `killRightMgr` | `fighterabilities.static`, `wrecks.static` | Target locking timer, Turret tracking / Missile DRF / Entropic spool / Vorton chain math, Shield$\rightarrow$Armor$\rightarrow$Hull, Loot Fairy & Wreck (`wrecks.static`), Pod ejection & death, Drones & Squadron Fighters, CrimeWatch flags & CONCORD |
| **12. Markets, Wallet, Contracts & LP Store** | `marketProxy`, `account`, `contractProxy`, `contractProxy`, `trademgr`, `LPStore` | `marketgroups.fsdbinary`, `accountingentrytypes`, `accountingkeys` | Regional bid/ask order book, immediate vs limit matching, Broker Fee & Sales Tax formulas, Escrow, Item Exchange/Courier/Auction contracts, Wallet Journal |
| **13. Industry, Blueprints, Reactions, Reprocessing & PI** | `ramProxy`, `reprocessingSvc`, `planetMgr`, `moonExtractions` | `blueprints.static`, `industry_*.static`, `typematerials.fsdbinary`, `schematics.fsdbinary`, `compressibletypes` | Manufacturing ($ME$/$TE$), Research, Copying, Invention (Datacores/Decryptors), Refinery Reactions, Ore/Ice/Scrap Reprocessing, Fleet Compression, Planetary Industry colonies |
| **14. Agents, Missions, Dungeons, Epic Arcs & NodeGraphs** | `agentMgr`, `missionMgr`, `dungeonExplorationMgr`, `air_npe`, Quasar `jobboard` | `missions.fsdbinary`, `dungeons.fsdbinary`, `epicarcs.fsdbinary`, `nodegraphs.fsdbinary`, `objectivechains`, `conversations`, `npccharacters` | Agent dialogue & standings checks, Courier/Kill/Mining/Encounter missions, multi-room `dungeons.fsdbinary` acceleration gates & 39 room triggers, 9 Epic Arcs, NPE NodeGraph runner |
| **15. NPC AI & Behavior Trees** | `SystemActor` AI (`NPCAI` + `behaviors/`) | `dungeons.fsdbinary`, `spacecomponentsbytype.fsdbinary` | Belt rat spawning, NPC target selection, orbit/keep-at-range kinematics, NPC weapon/EWAR cycles, Tier 2 Behavior-Tree/Blackboard AI, bounty payouts (`entityBounty` + Dynamic Bounty System) |
| **16. Social, Chat (`LSC`), Mail, Bookmarks & Fleets** | `LSC`, `mailMgr`, `notificationMgr`, `bookmark`, `fleetMgr`, `fleetObjectHandler` | — | Local/Corp/Alliance/Custom chat channels, EVE Mail & Notifications, 3D Bookmarks & Shared Folders, Fleet hierarchy (Boss/Wing/Squad), Fleet Warp, Watchlist, Command Bursts |
| **17. Corporations, Alliances, Standings, Wars & Sov** | `corporationSvc`, `corpRegistry`, `allianceRegistry`, `standingMgr`, `warRegistry`, `facWarMgr`, `sovMgr` | `npccorporations`, `corporationroles`, `factionsowningsolarsystems.static`, `warzonespositiondata` | Player corp creation, roles/titles, 7 corp hangar/wallet divisions, standings transactions, Mutual/HQ Wars, Faction Warfare capture plexes, Sovereignty Hubs & Skyhooks |
| **18. Exploration, Probe Scanning, Wormholes & Abyssal** | `scanMgr`, `wormholeMgr`, `abyssalMgr`, `pvpFilamentMgr` | `voidspaceencounters.fsdbinary`, `pvpfilamentmatchtypes.fsdbinary` | Directional Scanner (D-Scan), 8-probe 3D trilateration math, Relic/Data Hacking node-graph minigame, Wormhole mass exhaustion & polarization, 20-minute 3-pocket Abyssal Deadspace |

---

### 3.5 Additional Subsystems Required for Full V24.01 Client Parity

A programmatic diff of all **`217` cataloged remote services** (`193` active V24.01 + `24` Crucible legacy) in [`docs/index/rpc_service_catalog.json`](../index/rpc_service_catalog.json) and all **`135` Quasar Protobuf domains** in [`docs/index/quasar_protobuf_catalog.json`](../index/quasar_protobuf_catalog.json) identified 14 additional subsystems included in the domain architecture:

1. **Direct Player-to-Player Station Trading (`trademgr`):**
   - Live two-party trade session (`InitiateTrade`, `Add`, `OfferMoney`, `ToggleAccept`, `CancelTrade`) executing an atomic swap of items and ISK between two characters docked in the same station or Upwell structure (`source: eve/client/script/ui/shared/trade/tradeSvc.pyj`).
2. **HyperNet Relay / Raffles (`raffleMgr`, `raffleProxy`, `raffleProxy`, Quasar `eve_public.raffle`):**
   - Escrows a player item + `HyperCores` (`typeID = 52568`), splits into $8, 16, 48,$ or $512$ tickets, handles ticket purchases (`BuyTickets`), and draws a winner once all tickets sell (or refunds after 72 hours).
3. **Kill Rights, Bounty Office & CONCORD Security Office (`killRightMgr`, `bountyProxy`, `crimewatch`):**
   - **Kill Rights (`killRightMgr`):** Awarded on criminal Highsec aggression or Lowsec pod kills (`GetMyKillRights`, `ActivateKillRight`; full rules in `Section 3.11.6`).
   - **Security Office (`crimewatch`):** Turning in Clone Soldier Tags (`Transporter`, `Negotiator`, `Trainer`, `Recruiter`) + ISK at Lowsec CONCORD stations (`CanAccessServiceInStation`, `RegainSecurityStatus`; full rules in `Section 3.10.4.6`).
4. **Access Lists (ACLs) & Shared Bookmark Folders (`accessGroupBookmarkMgr`, `corpBookmarkMgr`, Quasar `eve_public.acl`):**
   - Access Group ACL management (Admin/Manager/Member/Blocked) powering both Upwell Structure permissions and Shared Bookmark Folders (`accessGroupBookmarkMgr`, up to 3 personal + 3 shared active folders with 3h/2d/permanent expiry).
5. **In-Space & Structure Ore/Ice/Gas Compression (`inSpaceCompressionMgr`, `structureCompressionMgr`):**
   - Industrial Command Ships (Rorqual, Orca, Porpoise) running Compressor modules grant fleetmates `inSpaceCompressionMgr.CompressItemInSpace` to compress raw asteroid ore, moon ore, ice, and gas (`compressibletypes.fsdbinary`) in space, while Upwell structures provide `structureCompressionMgr.CompressItemInStructure`.
6. **Item Repackaging, Repair & Salvaging (`repackagingSvc`, `repairSvc`):**
   - `repackagingSvc.RepackageItems` (resets assembled singleton items/ships to stackable `singleton = 0`, destroying rigs except T3 Cruiser subsystems), `repairSvc.GetDamageReports`/`RepairItems` (station ISK repair & free Upwell tether repair), and Salvager module cycles converting wrecks into salvage materials.
7. **Asset Safety & Deliveries Hangar (`inventoryAssetHoldingMgr`, `structureAssetSafety`, Quasar `eve_public.assetholding`):**
   - Wraps personal/corp items into an `AssetSafetyWrap` when an Upwell Structure is unanchored/destroyed or manually triggered (`GetItemsInSafetyForChar`, `MoveEjectedItemsToStructure`; full rules in `Section 3.11.5`), plus the `flagDeliveries` (`173`) hangar.
8. **Redeeming Queue, Expert Systems, Alpha Injectors & Login Campaigns (`userSvc`, `expertSystemMgr`, `alphaInjectorMgr`, `nonDiminishingInjectionMgr`, `rewardMgr`, `seasonalLoginCampaignManager`):**
   - Account/character redeeming item queue (`redeem.GetRedeemTokens`, `ClaimRedeemTokens`), 7-day temporary skill rental bundles (`expertSystemMgr`), Daily Alpha Injectors (`alphaInjectorMgr`), and Seasonal Login Reward tracks (`communityTrackMgr`).
9. **Corporation & Freelance Projects, Opportunities & AIR Career Program (Quasar `corporationgoal`, `freelance`, `career`, `dailygoal`, `opportunity`, `milestoneMgr`, `activityMgr`):**
   - **Corp & Freelance Projects (`corporationgoal`, `freelanceprojects`):** Automated corp/public projects (full specification in `Section 3.11.7`).
   - **AIR Career Program & Daily Goals (`careergoals`, `dailygoal`, `checkpoints.fsdbinary`, `goals.fsdbinary`):** Tracks career milestones and the 4 daily tasks (full specification in `Section 3.8.5`).
10. **Pirate Insurgencies, Corruption/Suppression & Interstellar Shipcasters (`insurgencySolarsystem`, `shipCasterLauncherMgr`, `shipcasterTravelMgr`, `starterShipcasterTravelMgr`, `mercenaryDen`):**
    - Angel Cartel & Guristas Insurgency FOBs (`insurgencySolarsystem`), dynamic system Corruption/Suppression stages 1–5 (`insurgencySolarsystem`), and Faction Warfare Interstellar Shipcasters (`shipcasterTravelMgr`; full specification in `Section 3.7.7`).
11. **Equinox Sovereignty Hubs, Orbital Skyhooks & Metenox Moon Drills (`orbitalSkyhook`, `sovHub`, `autoMoonMiner`, `colonyResourcesMgr`, Quasar `eve_public.sovereignty`):**
    - Planetary Power/Workforce/Reagent harvesting via `orbitalSkyhook` (including reagent silo raiding via `linkWithShip`), `sovHub` upgrades, and automated `autoMoonMiner` (Metenox Moon Drill; full specification in `Section 3.7.8`).
12. **Triglavian Pochven, Observatory Flashpoints & Solar System Interference (`triglavian`, `taleMgr`, `solarsystemInterferenceMgr`):**
    - Standing-gated Pochven stargates/stations, dynamic invasion/event tales (`taleMgr`), and `solarsystemInterferenceMgr.GetSolarsystemInterferenceStateNow` affecting cyno/CRAB interference.
13. **Calendar, Mailing Lists, Contacts, Medals, Notes, Search & Server-Side Settings (`calendarMgr`, `mailingListsMgr`, `search`, `lookupSvc`, `overviewPresetMgr`, `sharedSettingsMgr`, `characterSettingsStorage`, `kiringMgr`, `voucher`):**
    - In-game Calendar events/RSVPs (`calendarMgr`/`calendarProxy`), Mailing Lists (`mailingListsMgr`), Corp Medals (`medalMgr.CreateMedal`/`GiveMedal`), universal `search`/`lookupSvc`, and server-persisted UI settings/overview presets (`characterSettingsStorage`, `overviewPresetMgr`, `sharedSettingsMgr`).
14. **Survey Scanners, Moon Survey Probes, Mining Ledger, Project Discovery & Tournament Arenas (`miningScanMgr`, `moonExtractions`, `characterMiningLedger`, `corpMiningLedger`, `ProjectDiscovery`, `tourneyMgr`, `tourneyMgr`):**
    - Asteroid Survey Scanner (`miningScanMgr`), Moon Composition Probes (`moonExtractions`), Personal & Corp Mining Ledgers (`characterMiningLedger`, `corpMiningLedger`), Project Discovery minigame (`ProjectDiscovery`), and Tournament match rules (`tourneyMgr`).

---

