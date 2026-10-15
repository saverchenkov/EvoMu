# EvoMu Deep-Dive Spec 04: Storylines, Quests, Missions, Career Agents, 9 Epic Arcs & NPE Tutorial Engine (`§3.8`)

> Part of the [`PLAN.md`](../../PLAN.md) Master Specification.

---

### 3.8 Complete Storylines, Quests, Missions, Career Agents, Epic Arcs, AIR Career Program & NPE Tutorial Engine Specification

Cross-referencing V24.01 Client bytecode (`code.ccp`), client staticdata archives (`res:/staticdata/*.fsdbinary`), and `~/ai/eve-wiki` reveals **8 distinct narrative, quest, tutorial, and mission engines** in modern EVE Online. Every layer is data-driven by static FSD binary graphs in the client and orchestrated by specific server RPCs and notifications:

#### 1. Static Data Archives Driving All Tutorials, Quests, Missions & Storylines (`res:/staticdata/`)
| StaticData Archive | Size | Role in `EvoMu` Narrative & Mission Engine |
|---|---:|---|
| `missions.fsdbinary` | `1,288,248 B` | Master catalog of all Missions (`content_id`, `nameID`, `descriptionID`, `important` [Storyline flag], `nodeGraphID`, `clientObjectives`, `objectiveChainID`, `killMission`, `courierMission`, `initialAgentGiftTypeID`). |
| `dungeons.fsdbinary` | `52,395,840 B` | Complete multi-room pocket definitions, spawn groups, waves, Acceleration Gate lock conditions, and room triggers (`dunTrigger*` / `dunEventMessage*`). |
| `dungeonobjectives.fsdbinary` | `8,024 B` | Dungeon room objective state definitions (`dunObjectiveData`). |
| `epicarcs.fsdbinary` | `16,692 B` | Directed Acyclic Graphs (DAGs) of chapters and mission nodes for all 9 Epic Arcs (`EPICARC_SOE = 29` through `EPICARC_AIR = 132`). |
| `nodegraphs.fsdbinary` & `nodetypes.fsdbinary` | `8,277,272 B` | Client/Server visual execution state machines (`node_graph`) driving NPE scenes, UI pointer highlights, Aura voiceovers, and mission objective trackers. |
| `objectivechains.fsdbinary`, `objectives.fsdbinary` & `objectivetypes.fsdbinary` | `300,280+ B` | Structured multi-step objective trees displayed in the Info Panel and Opportunities (`jobboard`) window. |
| `conversations.fsdbinary` & `conversationagents.fsdbinary` | `328,199 B` | Spoken & subtitled NPC dialogue lines for Aura, Balin, Vesper, and Storyline/Epic Arc characters. |
| `npccharacters.fsdbinary`, `agentsinspace.fsdbinary` & `agenttypes.fsdbinary` | `1,885,514 B` | All NPC Agents (`agentID`, `corporationID`, `divisionID`, `level`, `stationID` or space beacon `solarSystemID`, `agentTypeID`). |
| `careerpaths.fsdbinary`, `careerpathgoals.fsdbinary`, `checkpoints.fsdbinary` & `goals.fsdbinary` | `18,523 B` | NPE checkpoints (`12..92`), Career Path definitions (*Enforcer*, *Explorer*, *Industrialist*, *Soldier of Fortune*), and `188` individual AIR Career Program goal records. |

#### 2. Layer 1: The AIR New Player Experience (NPE Storyline Tutorial — `storylines/` & `air_npe`)
- **Client Modules (`source: code.ccp`):** `storylines/common/airnpeconstants.pyj`, `storylines/client/airnpe.pyj`, `storylines/client/objectives/trackers/airnpetracker.pyj`, `jobboard/client/features/storylines/`.
- **Remote Service & Notifications (Verified in `docs/index/rpc_service_catalog.json` via `airnpe.pyj`):**
  - `sm.RemoteSvc('air_npe')`:
    - `is_air_npe_enabled() -> bool`
    - `get_air_npe_state() -> int` (`AirNpeState.ACTIVE = 1`, `COMPLETED = 2`, `SKIPPED = 3`, `DISABLED = 0`)
    - `skip_air_npe() -> None`
  - ScatterEvent: `OnAirNpeStateChanged(character_id, new_state_id)` + `OnCharacterEnteredFirstRookiesystem`.
- **Two-Act Dramatic Structure & NodeGraphs (`Acts 1–2`, 10 Scenes):**
  - Root NodeGraphs: `AIR_NPE_NODE_GRAPH_ID = 716`, `AIR_NPE_CLIENT_NODE_GRAPH_ID = 58745`, `AIR_NPE_ACT_1_NODE_GRAPH_ID = 707`, `AIR_NPE_ACT_2_NODE_GRAPH_ID = 708`, `NES_INTRO_NODE_GRAPH_ID = 745`.
  - **Act 1 (The Attack on the AIR Research Facility — 5 Scenes, 19 Checkpoints):**
    - `SCENE_1_1` (*Awakening in the Wreckage*, Checkpoints `12..17`): Camera orbit/zoom, approaching the AIR station (`approach_station`), Aura resuscitation dialogue.
    - `SCENE_1_2` (*Combat Defense*, Checkpoints `18..23`): Locking targets (`lock_target`), activating civilian weapons (`activate_weapons`), destroying hostile wave (`destroy_enemies`).
    - `SCENE_1_3` (*EWAR & Propulsion*, Checkpoints `24..26`): Activating Afterburner (`activate_afterburner`), Stasis Webifier (`activate_webifier`), destroying the enemy reinforcement wave (`destroy_enemies_2`).
    - `SCENE_1_4` (*Catastrophic Ambush & Podding*, Checkpoints `27..29`): Warping to secondary site (`warp_to_site`), fighting until ship destruction (`destroy_ships` / forced ship loss), warping capsule to station (`warp_to_station`), and docking (`dock_at_station`).
    - `SCENE_1_5` (*Clone Rebirth & Boarding*, Checkpoints `30, 81`): Awakening in new clone inside station (`station_intro`) and boarding a new racial Frigate (`board_ship`).
  - **Act 2 (Journey to the Career Hub — 5 Scenes, 10 Checkpoints):**
    - `SCENE_2_1` (*Undock & Skill Injection*, Checkpoints `83..84`): Undocking (`undock`) and injecting/starting first skill (`start_skill`).
    - `SCENE_2_2` (*The Astero Gate Ambush*, Checkpoints `85..86`): Warping to the Acceleration Gate (`warp_to_gate`), clearing gate defenders (`destroy_enemies`), and activating the Acceleration Gate (`activate_gate`).
    - `SCENE_2_3` (*Fleet Rescue Battle*, Checkpoints `87..88`): Orbiting target (`orbit_target`) and destroying the boss wave alongside AIR fleet NPCs (`destroy_enemies`).
    - `SCENE_2_4` (*Stargate Jump to Starter Career System*, Checkpoints `89..90`): Warping to the Stargate (`warp_to_stargate`) and jumping into the Career Agent system (`jump_through_stargate`).
    - `SCENE_2_5` (*Arrival at Career Station*, Checkpoints `91..92`): Warping to the school/AIR Career Station (`warp_to_station`) and docking (`dock_at_station`), handing the player off to the **5 Career Agents** and **AIR Career Program**.

#### 3. Layer 2: The Exordium Starter Region (`source: Exordium.md, eve/common/lib/appConst.pyj`)
- **Safe-Sec (`1.0` Security) Starter Region (`regionExordium`):**
  - In V23.02/V24.01, traditional Highsec `1.0` systems were adjusted to `0.9`, reserving true **`1.0` Security** for **Exordium** (`Career_Agents.md`, `Exordium.md`).
  - **Absolute PvP Prevention (`CantEnterExordiumWhileFlagged`):** Inside Exordium, player safety is **locked to Green** (`CrimeWatch`), duels/limited engagements, corporation wars, and Factional Warfare are disabled, and capsuleers with active Suspect, Criminal, or Limited Engagement timers cannot jump into Exordium or undock until their timer expires.
  - **Entry/Exit & One-Time Empire Shipcasters (`starterShipcasterTravelMgr`):**
    - Primary stargate entry/exit is between **Manifest** (housing the *Manifest V - AIR Laboratories Trade Center*) and **Yulai**.
    - Four one-way, **once-per-character Shipcasters** fast-track graduating rookies directly into Empire Highsec:
      - *Tatrys* $\rightarrow$ *Onnamon* (Caldari)
      - *Mondaekus* $\rightarrow$ *Intaki* (Gallente)
      - *Dettisolin* $\rightarrow$ *Amo* (Minmatar)
      - *Ichorah* $\rightarrow$ *Mehatoor* (Amarr)
  - **Economic Guardrails in Exordium:** $+5\%$ additional market fee on non-instant orders, $+5\%$ additional industry installation fee (Manufacturing, ME/TE Research, Invention), $-20\%$ mission ISK/LP & NPC bounties, Grade 0 low-yield ores, and blocks on anchoring player Upwell structures/deployables.

#### 4. Layer 3: Career Agents (5 Career Tracks $\times$ 20 Stations = 100 Agents; 45–50 Missions per Set, `420` Tutorial/Career Entries in `missions.fsdbinary`)
- **Client Integration:** `agentTypeCareerAgent = 9` uses the modern `agentinteraction/` UI window (`AGENT_TYPES_WITH_NEW_UI` in `eve/client/script/ui/station/agents/agents.pyj`). Client calls `sm.RemoteSvc('agentMgr').GetCompletedCareerAgentIDs()` to track completed agents in *The Agency* (`Alt+M`) and AIR Career Program.
- **20 Career Agent Stations (12 Classic Empire School Stations + 8 Exordium Stations):**
  - *Amarr:* Conoban, Pasha, Deepari + Exordium (*Kanta IV*, *Mazor V*).
  - *Caldari:* Akiainavas, Jouvulen, Uitra + Exordium (*Pamirasu VI*, *Aldoras VI*).
  - *Gallente:* Clellinon, Couster, Trossere + Exordium (*Ligare V*, *Rotonos IV*).
  - *Minmatar:* Malukker, Hadaugago, Embod + Exordium (*Dranga III*, *Tungna IV*).
- **5 Non-Exclusive Career Mission Chains (Completable in Parallel):**
  1. **Enforcer (`Cash Flow for Capsuleers` 1/10 – 10/10):** Basic PvE combat, stasis webifiers, warp scramblers, deadspace pockets; rewards combat Frigate, skillbooks, modules, and $+1.5\%$ Faction Standing.
  2. **Soldier of Fortune (`1/10 – 10/10`, including `Angel of Mercy`, `Your Undivided Attention`, `The Stand`, `The Exam`):** Advanced combat, EWAR, ship insurance, intentional ship sacrifice tutorials (missions 2/10 and 5/10 script-destroy the player's ship to teach capsule survival & insurance payouts), culminating in a Tech 1 **Destroyer** reward and $+1.5\%$ Faction Standing.
  3. **Explorer (`1/5 – 5/5` or `1/10 – 10/10` variant in `missions.fsdbinary`):** Cosmic signatures, Core Probe scanning, Data Site hacking, Relic Site archaeology, and Gas Cloud harvesting; rewards Exploration Frigate (`Imicus`/`Heron`/`Magnate`/`Probe`) and $+1.5\%$ Faction Standing.
  4. **Industrialist - Entrepreneur (`Balancing the Books` 1/10 – 10/10):** Hauling, salvaging, mining, hacking, market buying/selling, and manufacturing; rewards a **Venture** mining frigate, Salvager, and $+1.5\%$ Faction Standing.
  5. **Industrialist - Producer (`Making Mountains of Molehills` 1/10 – 10/10):** Mining Veldspar/Kernite, ore reprocessing into Tritanium, blueprint manufacturing, and turning in a manufactured T1 Frigate; rewards a T1 **Hauler** (`Bestower`/`Badger`/`Iteron Mark V`/`Wreathe`) and $+1.5\%$ Faction Standing.
- **V23.02+ Standing Rule:** Completing the final mission of each Career Agent grants $+1.5\%$ Faction Standing ($+7.5\%$ per station set) **with zero negative Derived Standings** (matching Epic Arcs) and awards a 3-day **AIR Expert System**.

#### 5. Layer 4: AIR Career Program & Daily Goals (`careerpathgoals.fsdbinary` & Quasar `eve_public.career.goal` / `dailygoal`)
- **AIR Career Program (`AIR_Career_Program.md`, `careerpathgoals.fsdbinary`):**
  - Organized into **4 Career Categories** (*Enforcer*, *Explorer*, *Industrialist*, *Soldier of Fortune*) with ~10 activity groups per path, where each activity group contains multiple tiered sub-goals totaling **`188` individual goal records** in `careerpathgoals.fsdbinary` worth $4,000\text{ Career Points (CP)}$ ($1,000\text{ CP}$ per career).
  - **Rewards:**
    - Individual goal completion: $12,500–125,000\text{ ISK}$, Career Points, faction-specific Ship/Skill/Blueprint/Filament Crates (`typeID 63780`, `63782`, etc.), and Boosters.
    - Graduating a Career ($750\text{ CP}$ in category): $100,000\text{ SP}$ ($\times 4 = 400,000\text{ SP}$) + Empire SKINs.
    - Overall CP Milestones ($750 / 1,500 / 2,250 / 3,000\text{ CP}$): $+50\text{k} / +75\text{k} / +100\text{k} / +125\text{k SP}$ ($350,000\text{ SP}$), totaling **$750,000\text{ SP}$ for Alpha + $750,000\text{ SP}$ for Omega ($1,500,000\text{ SP}$ total)**.
- **AIR Daily Goals (`dailygoals/`):**
  - 4 daily generated tasks (*Manufacture an item*, *Scan 5 signatures*, *Destroy 25 non-capsuleers*, *Mine 2,000 units of ore*); completing any 2 grants the daily $10,000\text{ SP}$ + EverMarks + ISK reward and advances the 12-milestone monthly reward track.

#### 6. Layer 5: Standard Agent Missions & The 39-Trigger Dungeon State Machine (`svc.agents`, `agentMgr`, `dungeons.fsdbinary`)
- **Client-Server Agent Moniker Protocol (`eve/client/script/ui/station/agents/agents.pyj`):**
  1. Client binds agent moniker via `eveMoniker.GetAgent(agentID, stationID)` (`agentMgr`).
  2. Client calls `agentMoniker.GetAgentRegularInfo()` and `InteractWithAgent(actionID)` where `actionID` is one of:
     - `agentDialogueButtonViewMission = 1`, `RequestMission = 2`, `Accept = 3`, `Reject = 4`, `Defer = 5`, `Quit = 6`, `StartResearch = 7`, `BuyDatacores = 8`, `LocateCharacter = 9`, `AcceptChoice = 10`, `AcceptRemotely = 11`, `Complete = 12`, `CompleteRemotely = 13`, `Continue = 14`.
  3. `GetMissionBriefingInfo()` returns a `KeyVal` containing `ContentID` (referencing `missions.fsdbinary`), `MissionKeywords` (template dictionary substituting system names, item types, dropoff stations, and NPC targets into the FSD briefing), `AcceptTimestamp`, `ExpirationTime`, `BonusTime`, `MissionImage`, and `Bookmarks` (Dungeon entry warp coordinates / Station pickup/dropoff bookmarks).
  4. Server emits `OnAgentMissionChange(actionID, agentID)` (`offered`, `accepted`, `completed`, `declined`, `quit`, `offerRemoved`, `talkToCompleted`).
  5. Active client tracker (`missions/client/active_agent_mission.pyj`) loads `get_mission(content_id)` from `missions.fsdbinary`, activates `nodeGraphID` + `objectiveChainID`, and listens for `OnDungeonEntered(dungeon_id, instance_id)` / `OnDungeonExited(dungeon_id, instance_id)`.
- **The 39 Dungeon Room Event Triggers (`evedungeons/common/triggers.pyj` & `missions/common/missionTriggerTypeNames.pyj`):**
  Every multi-room pocket in `dungeons.fsdbinary` is an event-driven state machine inside `SystemActor` that evaluates 39 canonical trigger types:
  - *Combat & Damage Triggers:* `dunTriggerAttacked (1)` (aggro group attacked), `dunTriggerArmorConditionLevel (2)`, `dunTriggerShieldConditionLevel (3)`, `dunTriggerStructureConditionLevel (4)`, `dunTriggerExploding (6)` (specific entity destroyed), `dunTriggerGroupWipedOut (22)`, `dunTriggerRoomWipedOut (11)` (all hostiles in room destroyed), `dunTriggerRoomCaptured (20)`, `dunTriggerRoomAttacked (21)`, `dunTriggerShipsLeft (35)`.
  - *Spatial & Gate Triggers:* `dunTriggerShipEnteredProximity (5)` (player enters sphere around beacon/gate), `dunTriggerShipEnteredRoom (32)`, `dunTriggerPlayerWarpPursuit (14)`, `dunTriggerAttemptedGateActivation (38)` (checks keycard / ship hull class restriction).
  - *Inventory, Container & Cargo Triggers:* `dunTriggerItemPlacedInMissionContainer (7)`, `dunTriggerItemInCargo (17)`, `dunTriggerAllItemsRemovedFromSpawnContainer (27)`, `dunTriggerItemRemovedFromSpawnContainer (28)`.
  - *Harvesting & Exploration Triggers:* `dunTriggerMined (8)`, `dunTriggerRoomMinedOut (12)`, `dunTriggerHackingSuccess (18)`, `dunTriggerHackingFailure (19)`, `dunTriggerArchaeologySuccess (25)`, `dunTriggerArchaeologyFailure (26)`, `dunTriggerSalvagingSuccess (23)`, `dunTriggerSalvagingFailure (24)`.
  - *Timer, Counter & Effect Triggers:* `dunTriggerTimerComplete (34)`, `dunTriggerCounterEqual (29)`, `dunTriggerCounterGreater (30)`, `dunTriggerCounterLess (31)`, `dunTriggerEffectActivated (9)`.
  - *Trigger Actions (`dunEvent*`):* Unlocking/Locking an Acceleration Gate, spawning a reinforcement wave (`spawnGroupID`), broadcasting an NPC chat/transmission (`dunEventMessageEnvironment`, `dunEventMessageMissionInstruction`, `dunEventMessageMood`, `dunEventMessageStory`, `dunEventMessageWarning`), granting/completing a mission objective (`dunObjectiveData`), or exploding a structure/ship.

#### 7. Layer 6: Storyline Missions (`Storyline_missions.md`)
- **The 16-Mission Tally Rule:**
  - `eve-db` tracks `mission_tally(char_id, faction_id, mission_level)` across all regular Security, Distribution, Mining, R&D, and Epic Arc missions.
  - Upon completing the **16th mission** of a given `(faction_id, mission_level)` pair (across any corporations belonging to that faction), the server selects the nearest `agentTypeStorylineMissionAgent = 3` of that faction (by stargate jump count from the 16th mission's agent, constrained to Highsec if the 16th agent was in Highsec, and skipping any Storyline Agent who already has an active pending offer for the player).
  - The Storyline mission's difficulty/reward level matches the `mission_level` ($1..5$) of the 16 completed missions (even though all Storyline Agents have `agentLevel = 1` for universal access).
  - Completing a Storyline mission awards large Corporation + Faction Standings **with full positive and negative inter-faction Derived Standings** (`Section 3.6.16`), plus Storyline (`Meta 6–7`) Modules or Attribute Implants.

#### 8. Layer 7: The 9 Branching Epic Arcs (`svc.epicArc`, `epicarcs.fsdbinary`, `Epic_arcs.md`)
- **Client Service (`eve/client/script/ui/shared/epicArcSvc.pyj`):**
  - Calls `sm.RemoteSvc('agentMgr').GetMyEpicArcStatus()` returning completed/active mission nodes and timestamps for each arc in `epicarcs.fsdbinary`, and listens for `OnEpicJournalChange`.
  - Each Epic Arc node wraps a mission from `missions.fsdbinary` and transitions through states: `OFFERED`, `ACCEPTED`, `COMPLETED`, `DECLINED`, `FAILED`, `QUIT`. Branching nodes (`Agent Interaction` missions) let the player pick one of $2..4$ mutually exclusive faction/choice paths.
  - **90-Day Cooldown & Standing Immunity:** Epic Arc missions never expire once offered, can be replayed every **90 days** after completion or failure, and grant large Faction Standings **with zero negative Derived Standings** (shareable with fleetmates).
- **Complete Catalog of All 9 Epic Arcs (`eve/client/script/ui/shared/epicArcs/epicArcConst.pyj`):**
  | `epicArcID` | Constant | Epic Arc Name | Faction | Level | Missions | Starting Agent & Location | Signature Rewards |
  |---:|---|---|---|---:|---:|---|---|
  | `29` | `EPICARC_SOE` | **The Blood-Stained Stars** | Sisters of EVE | L1 | 51 | Sister Alitura (*Arnon IX - Moon 3 - SOE Bureau*) | $+0.70$ ($+0.875$ w/ Social V) to chosen Empire Faction |
  | `40` | `EPICARC_CALDARI` | **Penumbra** | Caldari State | L4 | 17 | Aursa Kunivuri (*Josameto*) | $+10\%$ Caldari Standing + *Hyasyoda Research Lab* BPC or *25,000x* Hybrid Charges |
  | `48` | `EPICARC_AMARR` | **Right to Rule** | Amarr Empire | L4 | 18 | Karde Romu (*Nishah VII - Moon 5*) | $+10\%$ Amarr Standing + *Imperial Navy Modified 'Noble' Implant* |
  | `52` | `EPICARC_GALLENTE` | **Syndication** | Gallente Federation | L4 | 19 | Roineron Aviviere (*Dodixie IX - Moon 20*) | $+10\%$ Gallente Standing + *Black Eagle Drone Link Augmentor* |
  | `53` | `EPICARC_MINMATAR` | **Wildfire** | Minmatar Republic | L4 | 18 | Nilf Abruskur (*Frarn*) | $+10\%$ Minmatar Standing + *Republic Fleet Target Painter* |
  | `55` | `EPICARC_GURISTAS` | **Smash and Grab** | Guristas Pirates | L3 | 12 | Kataikooinokka (*6-CZ49* Nullsec / Lowsec entry) | $+30\%$ Guristas Standing + *Gistii A-Type* modules & *Gila* BPC |
  | `56` | `EPICARC_ANGEL` | **Angel Sound** | Angel Cartel | L3 | 11 | Lutins (*Utopia* Curse Nullsec / Lowsec entry) | $+30\%$ Angel Standing + *Domination* modules & *Cynabal* BPC |
  | `128` | `EPICARC_ORE` | **Fractured Legacy** *(Catalyst)* | ORE | L1 | 3 | Elias Peltonnen (*Kisogo VII - AIR Laboratories*) | **Pioneer** Mining Destroyer + 1-run *Mobile Phase Anchor* BPC |
  | `132` | `EPICARC_AIR` | **Vision of Greatness** *(Exordium)* | AIR Laboratories | L1 | 8 | Cassandra Ahfrim (*Manifest V - AIR Trade Center*) | $+2.5\%$ Faction Standing to **all 4 Empires** (Amarr, Caldari, Gallente, Minmatar) |

#### 9. Layer 8: Static World Quests — COSMOS (`agentsinspace.fsdbinary`), Data Centers & Circle Agents
- **COSMOS Agents in Space (`agentsinspace.fsdbinary`, `COSMOS.md`):**
  - 246 NPC Agents spawned as persistent ships floating inside space beacons across Highsec (*Ani*, *Araz*, *Okkelen*, *Algintal*), Lowsec (*Kenobanala*, *Audesder*, *Tartatven*), and Nullsec (*Delve*, *Feythabolis*, *Vale of the Silent*, *Fountain*, *9HXQ-G*, *09-4XW*, *Assilot*, *760-9C*).
  - Require **Faction Standing** (`0.0` for L1, `2.0` for L2, `4.0` for L3, `6.0` for L4; `8.5+` for 2-run Faction Ship BPC turn-ins).
  - Strictly **one-time per character**: offering starts a 7-day timer, and failing or expiring permanently locks the agent's 3–5 mission chain.
- **Data Center Tag Turn-Ins (`Data_center_missions.md`) & Circle Agent Referral Loops (`Circle_missions.md`):**
  - 23 Data Center agents accept 20x Pirate Dog Tags (Copper through Platinum) or Rookie Graduation Certificates for instant one-time Faction Standing boosts.
  - Circle Agents form 5-to-8 agent courier referral loops per Empire where each agent's mission delivers cargo to the next agent in the circle.

---

