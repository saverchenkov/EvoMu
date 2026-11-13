# Standings

> **Navigation**: [EvoMu Technical Wiki](../README.md) • [Related Systems](#related-systems--cross-references)
>
> [← Back to Wiki Table of Contents](../README.md)

---

## Overview

A **standing** is a number from −10.0 to +10.0 that records how one entity regards another. EvoMu has two unrelated kinds:

- **NPC standings**: how an NPC agent, NPC corporation or faction regards a character. They change through gameplay and gate access to agents, stations, loyalty-point stores and some regions. They are stored on the character.
- **Contact standings**: a value a player, corporation or alliance assigns to someone else in its contact list. They are stored in separate contact lists and never change by themselves.

The standing formulas, the agent access thresholds and the mission penalty table are described in [Agent Dialogue, Missions, Standings & LP Stores](../missions_and_agents/agent_dialogue_and_missions.md). This article does not repeat them. It covers where standings are stored, every code path that changes them, how effective and derived standings are computed, and every place that reads them to allow or deny something.

Security status is a different value (`CharacterRecord.security_status`) and is covered in [Crimewatch & Engagement Timers](../combat_and_crimewatch/crimewatch_and_engagement_timers.md).

---

## Canonical Mechanics

### Storage model

NPC standings toward a character are one map on the character: `CharacterRecord.standings`, from entity ID to raw standing. The key can be an agent ID, an NPC corporation ID or a faction ID. A missing key means no standing has been earned. There is no separate table for corporation-level NPC standings.

Changes are logged per character in `WorldStateSnapshot.standing_transactions` as `StandingTransactionRecord { event_id, from_id, to_id, modification, event_type_id }`.

### Effective standing

Raw standings are adjusted by skills before most checks. `compute_effective_standing_breakdown` returns the effective value and the bonus from each skill:

| Case | Bonus | Skill |
|---|---|---|
| Entity is Triglavian or EDENCOM (factions 500026, 500027, or their corporations and agents) | none; raw value clamped to ±10 | — |
| Raw standing below 0 | `(10 − raw) × 0.04 × level` | Diplomacy (3357) |
| Raw standing above 0, or an entry exists at 0, and the entity is a pirate (factions 500010, 500011, 500012, 500019, 500020, or their corporations and agents) | `(10 − raw) × 0.04 × level` | Criminal Connections (3361) |
| Raw standing above 0, or an entry exists at 0, any other entity | `(10 − raw) × 0.04 × level` | Connections (3359) |
| No entry | 0 | — |

For an agent, `compute_effective_agent_standing` takes the effective standing of whichever of the agent, its corporation and its faction have an entry. If any of them is −2.0 or lower the result is the lowest; otherwise it is the highest. With no entries the result is 0.

### What changes NPC standings

| Event | Code | Change | Transaction logged |
|---|---|---|---|
| Mission completed | `apply_mission_completion_standings` | corporation and agent gain; faction gain with derived changes (below) | one per changed entity; `event_type_id` 85 for a gain, 83 for a loss |
| Accepted mission quit | `abandon_or_decline_mission` | corporation −0.1, agent −0.1 | one, from the corporation, type 83 |
| Offer declined within 14,400 s of the previous decline (not R&D agents) | `abandon_or_decline_mission` | corporation −0.05, agent −0.05 | one, from the corporation, type 83 |
| Accepted mission expired | `agentMgr` dialogue handler in `agents_missions.rs` | agent −0.10, corporation −0.10, faction −0.05 | one, from the corporation, −0.10, type 84 |
| Faction warfare battlefield won | `tick_fw_gameplay` | militia faction and its LP corporation raised with `apply_standing_increase` | none |
| Triglavian or EDENCOM site completed | `apply_triglavian_standings_and_isk` | one of the two factions up, the other down (below) | none |
| GM command `/standing <targetID> <standing>` (also `/contact`) | `handle_gm_slash_ext` | sets the value directly when the target is an NPC corporation or faction | none |

Details:

- **Mission completion.** The mission's base percentage is multiplied by `1 + 0.05 × Social level`. The corporation and the agent each gain that percentage through `apply_standing_increase`. The faction gains 15 % of it, or 50 % for storyline and special missions.
- **Mission penalties are flat subtractions** clamped at −10; they do not use `apply_standing_decrease`.
- **Battlefield.** The code passes `0.15 × (1 + 0.05 × Social level)` to `apply_standing_increase`, which treats its argument as a percentage of the distance to +10.
- **Triglavian/EDENCOM sites.** The gaining faction moves by `(10 − cur) × 0.015 × (1 + 0.05 × Social level)`; the losing faction moves by `−(10 + cur) × 0.015`.

Killing NPCs or players does not change standings in the code read for this article: none of the writers of `CharacterRecord.standings` is in the combat or death pipeline.

### Derived faction standings

When a mission changes a faction standing, `apply_faction_standing_with_derived` also adjusts the character's standing with every faction that has an opinion of that faction. Opinions come from the static table `SYNTHESIZED_NPC_FACTION_STANDINGS` (directed pairs for factions 500001–500020, plus the two Triglavian/EDENCOM pairs at −10; the source comment cites a Crucible 1.6 standings dump). Faction sizes come from the SDE `size_factor`, defaulting to 5.0.

For each secondary faction whose opinion of the primary faction is `rel`:

$$\text{modifier} = rel \times \frac{\text{size}_{primary}}{\text{size}_{secondary}} \qquad \text{derived \%} = \text{faction \%} \times \frac{\text{modifier}}{10}$$

| Primary change | `rel` | Effect on the secondary faction's standing |
|---|---|---|
| gain | positive | increases by the derived percentage, capped at `rel`; unchanged if already at or above `rel` |
| gain | negative | decreases by the derived percentage, floored at `rel`; unchanged if already at or below `rel` |
| loss | positive | decreases by the derived percentage |
| loss | negative | unchanged |

Derived changes are skipped for Epic Arc agents (agent type 10) and for the final mission of a career agent chain (agent type 12 with a title ending "(10 of 10)" or "(5 of 5)", or titled "Song Of Birds").

### Notifications

After a mission completion, `notify_standing_changes` sends the character `OnStandingSet(fromID, toID, newValue)` for the first change and `OnStandingsModified` with a list of `(fromID, toID, delta, −10.0, 10.0)` for all of them. It is called only from the mission completion path.

### Queries

| RPC | Returns | Handler |
|---|---|---|
| `standingMgr.GetCharStandings` | rows `(fromID, standing)` from the caller's map | `handle_clones_and_sheet` in `character_clones_implants.rs` |
| `standingMgr.GetStandingTransactions(fromID)` | rows `(eventID, fromID, toID, modification, eventTypeID)`, optionally filtered by sender | same |
| `standingMgr.GetStandingCompositions(fromID, toID)` | per entity: raw, effective, and the Connections, Diplomacy and Criminal Connections bonuses | `handle_extended_with_bound` in `agents_standings_arcs.rs` |
| `standingMgr.GetCorpStandings` | per entity, the average standing over the members of the caller's corporation that have an entry | same |
| `standingMgr.GetNPCNPCStandings` | the static faction table, plus +10 both ways between every NPC corporation and its faction | same |
| ESI `GET /characters/{character_id}/standings` | entries whose key is a faction, NPC corporation or agent, as `from_id`, `from_type`, `standing` | `character_routes.rs` |
| ESI `GET /corporations/{corporation_id}/standings` | sum of logged transactions whose `to_id` is the corporation | `corp_alliance_routes.rs` |
| Quasar `eve.corporation.standing.npc.GetAllRequest` | transactions keyed by the corporation, merged with corporation contacts | `quasar_corp_domain.rs` |

### What standings gate

| Content | Rule | Standing used | Code |
|---|---|---|---|
| Agent conversation and missions | level above 1 needs `(level − 1) × 2 − 1`; level 1 needs above −2.0 (agent types 7 and 12 exempt) | effective agent standing | `agents_missions.rs`; listing flag in `handle_get_agents` |
| R&D agent research | agent threshold, plus corporation standing of threshold − 2 for agents above level 1 | effective | `validate_rnd_research_access` |
| NPC station services | per-service minimum configured for the owner corporation or faction (`StandingTooLowForStationService`) | effective | `check_station_service_standing`, `evaluate_station_service_standing_gate` |
| NPC station docking | configured docking minimum | raw adjusted by `effective_standing_with_skills` | `validate_npc_station_docking_gate` |
| Docking in high security | refused for a non-capsule ship when faction standing is −5.0 or lower (or by security status) | raw, system faction | `is_outlaw_ship_docking_refused` |
| Navy response | faction navy spawns against ships whose pilot is at −5.0 or lower with the system's faction; only for factions 500001–500004, in systems of security 0.45 and above | raw | `check_and_spawn_faction_police` |
| EDENCOM NPC targeting | a character with negative standing toward the NPC's faction is hostile | raw | `is_candidate_hostile_to_edencom` |
| Jump clone installation at a station | owner corporation and faction standing must both be at least 0 (`InsufficientStanding`) | raw | `character_clones_implants.rs` |
| LP store | offers with `min_standing`; Triglavian/EDENCOM stores need at least 0; a militia corporation's store needs enlistment in that militia or non-negative effective corporation and faction standing | raw or effective, as listed | `handle_take_offer` |
| Faction warfare enlistment | standing with the militia faction must be at least 0 (`FwStandingTooLow`) | raw | `fw_enlistment.rs`, `sov_fw_insurgency.rs` |
| Militia rank | derived from standing with the character's war faction | raw | `calculate_militia_rank` callers |
| Market broker fee | `max(1.0, 3.0 − 0.3 × Broker Relations − 0.03 × max(faction, 0) − 0.02 × max(corp, 0))` percent | raw | `compute_broker_fee_rate` |
| Reprocessing tax at NPC stations | `0.05 − 0.0075 × standing`, clamped to 0–0.05 | higher of personal (with Connections) and corporation average, floored at 0 | `compute_reprocessing_rates_for_item_at` |
| Pochven home-system stargates (Kino, Niarja, Archee) | Triglavian standing at least 6.0 (`SystemCheck_JumpFailed_StandingsTooLow`) | raw, faction 500026 | `handle_cmd_stargate_jump` |
| Pochven filaments | same 6.0 threshold for filament types 56080–56081; fleet mates below it are left behind for those systems | raw | `resolve_pochven_filament_candidates`, `handle_random_jump_filament` |
| Customs office in Pochven | effective Triglavian standing at least 1.5 (`POCHVEN_CUSTOMS_OFFICE_MIN_TRIG_STANDING`) | raw with Diplomacy/Connections | `validate_pochven_customs_office_standing` |
| Observatory Flashpoint | lowest Triglavian standing among the caller and on-grid fleet members must be at least 0 | raw | `validate_observatory_access` |
| Player structure market, customs office and clone-jump fee | compare `standings[owner corporation]` with a threshold | raw, keyed by the owning player corporation | `market_acl.rs`, `resolve_character_orbital_access_and_tax`, `character_clone_jump_rules.rs` |

### Contact standings (player-set)

These are stored separately from NPC standings.

| Owner | Stored in | Set by | Who may set | Notification |
|---|---|---|---|---|
| Character | `esi_state.char_contacts` at run time, synchronised to `WorldStateSnapshot.char_contacts` by `sync_char_contacts_to_db` | `charMgr.AddContact`, `EditContact`, `EditContactsRelationshipID`, `DeleteContacts`, `BlockOwners`, `UnblockOwners` | the character | `OnPersonalContactsAdded`, `OnSetPlayerStanding` |
| Corporation | `WorldStateSnapshot.corp_contacts` | `AddCorporateContact`, `EditCorporateContact` | Diplomat or Director role (`CorpRoleDenied`) | `OnSetCorpStanding` |
| Alliance | `WorldStateSnapshot.alliance_contacts` | `allianceRegistry.EditAllianceContact` and related | executor corporation (`require_executor`) | `OnSetAllianceStanding` |

Each entry is a `SocialContactRecord` with `contact_id`, `contact_type`, `standing`, `is_watched`, `is_blocked` and `label_ids`. Standing is limited to −10..+10. A character may hold at most 1,024 contacts (`MaxContactsExceeded`).

Contact standings are read by: the CSPA exemption and block checks for mail and chat invites ([Mail & Chat](../corporations_and_governance/mail_and_chat.md)), the fleet finder minimum-standing filter, which uses the fleet leader's corporation contacts ([Fleets](../corporations_and_governance/fleets.md)), and chat channel visibility for blocked contacts.

### Not implemented / known gaps

- **No standing change from combat.** NPC kills and player kills do not touch `CharacterRecord.standings`.
- **Penalties are not pushed to the client.** `notify_standing_changes` runs only after mission completion; quit, decline, expiry, battlefield and Triglavian-site changes update the map without `OnStandingSet` or `OnStandingsModified`.
- **Incomplete transaction log.** Mission expiry changes three standings but logs one transaction. Battlefield, Triglavian-site and GM changes log none.
- **Contraband confiscation logs a standing transaction without changing the standing.** `scan_and_enforce_highsec_contraband` appends a −0.1 record from the system's faction (with event type 85, the same type used for mission gains) but does not modify the `standings` map.
- **Battlefield gain is a percentage.** The comment at the call site describes 0.15 as a raw standing gain; `apply_standing_increase` applies it as 0.15 % of the remaining distance to +10.
- **No stored corporation NPC standings.** `GetCorpStandings` averages member standings. The ESI and Quasar corporation-standing reads sum transactions addressed to a corporation ID, and every writer found records the character as `to_id`.
- **Player-corporation keys are read but not written.** Structure market access, customs office access and the clone-jump fee read `standings[owner corporation]`. No gameplay path stores a player corporation ID in that map; the GM command writes the map only for NPC corporations and factions.
- **Several effective-standing implementations.** `compute_effective_standing_breakdown`, `effective_standing_with_skills`, `formulas::compute_effective_standing`, and local copies in the reprocessing and Pochven customs-office checks differ: only the first applies Criminal Connections and the Triglavian/EDENCOM exemption.
- **Quasar standing commands use a separate map.** `eve.standing.api.IncreaseCommanded` and `DecreaseCommanded` move a value in `QuasarCommunityState.standings` by 1.0 per call for any character with an active session. That map is not `CharacterRecord.standings`.
- No GitHub issue matching "standing" was open when this article was written.

---

## EvoMu Implementation Architecture

### Crates involved

- [`eve-server`](../../../crates/eve-server): all standing logic and gates.
- [`eve-db`](../../../crates/eve-db): `CharacterRecord.standings` and `StandingTransactionRecord` in [`types.rs`](../../../crates/eve-db/src/types.rs); `SocialContactRecord` and the snapshot fields in [`snapshot_ext.rs`](../../../crates/eve-db/src/snapshot_ext.rs).
- [`eve-sde`](../../../crates/eve-sde): agents, NPC corporations, factions and `size_factor`.

### Primary handlers and dispatch

| File | Contents |
|---|---|
| [`agents_standings_arcs.rs`](../../../crates/eve-server/src/services/agents_standings_arcs.rs) | effective standing, derived standings, mission completion, notifications, the static faction table, `standingMgr` corporation / NPC-NPC / composition queries |
| [`formulas.rs`](../../../crates/eve-server/src/formulas.rs) | `apply_standing_increase`, `apply_standing_decrease`, `compute_effective_standing`, `required_standing_for_agent_level`, `compute_broker_fee_rate` |
| [`agents_missions.rs`](../../../crates/eve-server/src/services/agents_missions.rs), [`agents_missions_helpers.rs`](../../../crates/eve-server/src/services/agents_missions_helpers.rs) | agent access gate, expiry penalty, `abandon_or_decline_mission` |
| [`character_clones_implants.rs`](../../../crates/eve-server/src/services/character_clones_implants.rs) | `standingMgr.GetCharStandings`, `GetStandingTransactions`; jump clone standing check |
| [`station_structure_handlers.rs`](../../../crates/eve-server/src/services/station_structure_handlers.rs) | station service and docking gates |
| [`combat_npc_police.rs`](../../../crates/eve-server/src/services/combat_npc_police.rs) | navy response, docking refusal, contraband enforcement |
| [`lp_store.rs`](../../../crates/eve-server/src/services/lp_store.rs) | LP store standing requirements |
| [`fw_gameplay.rs`](../../../crates/eve-server/src/services/fw_gameplay.rs), [`fw_enlistment.rs`](../../../crates/eve-server/src/services/fw_enlistment.rs) | battlefield gain, enlistment gate |
| [`pve_special_sites_ops.rs`](../../../crates/eve-server/src/services/pve_special_sites_ops.rs) | Triglavian/EDENCOM site standings |
| [`character_contacts.rs`](../../../crates/eve-server/src/services/character_contacts.rs) | personal contacts |
| [`fleet_social_notify.rs`](../../../crates/eve-server/src/services/fleet_social_notify.rs) (`handle_corp_contacts_and_labels`), [`social_corp_fleet.rs`](../../../crates/eve-server/src/services/social_corp_fleet.rs) | corporation and alliance contacts |
| [`character_routes.rs`](../../../crates/eve-server/src/esi/character_routes.rs), [`corp_alliance_routes.rs`](../../../crates/eve-server/src/esi/corp_alliance_routes.rs) | ESI standings routes |

### State

| Location | Field | Contents |
|---|---|---|
| `CharacterRecord` | `standings: HashMap<i64, f64>` | raw standing from each entity toward the character |
| `WorldStateSnapshot` | `standing_transactions` | list of `StandingTransactionRecord` per character |
| `WorldStateSnapshot` | `char_contacts`, `corp_contacts`, `alliance_contacts` | `SocialContactRecord` lists per owner |
| `ServerContext.esi_state` | `char_contacts` | run-time copy of personal contacts |
| `QuasarCommunityState` | `standings` | separate `(from, to)` map used only by the Quasar standing commands |

### Tick integration

Standings have no tick and do not decay. The only tick-driven writer is the faction warfare battlefield payout inside `tick_fw_gameplay`. The navy response check `check_and_spawn_faction_police` reads standings each time it runs for a solar system.

---

## Invariants, Security & Edge Cases

- Every writer clamps the result to −10..+10.
- A standing of exactly 0 with an entry is treated differently from no entry: Connections (or Criminal Connections) applies to the first and not to the second.
- One bad relationship blocks an agent: an effective standing of −2.0 or lower with the agent, its corporation or its faction overrides the other two.
- Triglavian and EDENCOM standings ignore skill bonuses, so the Pochven thresholds compare against the raw value (the customs-office check applies its own local skill adjustment).
- `standingMgr.GetStandingCompositions` accepts a `toID`; when that character exists its standings are returned to the caller.
- The GM `/standing` command requires the target to exist as a character, corporation, alliance, NPC corporation or faction, and rejects non-finite values.
- Derived standings never push a secondary faction past that faction's own opinion of the primary faction.

---

## Verification, Tests & Parity References

Ledger rows (`ledger.json`): `feat:standings.npc-standings`, `feat:standings.standing-gain-missions`, `feat:standings.standing-loss`, `feat:standings.effective-standing`, `feat:standings.standing-compositions`, `feat:standings.corp-standings`, `feat:standings.npc-npc-standings`, `feat:standings.standing-notifications`, `feat:standings.trig-edencom-standings`, `feat:pve.mission-standing-gate`, `feat:pve.mission-faction-standing-loss`, `feat:structures.station-standing-check`, `feat:npc.npc-standings-aggression`, `feat:esi.character.standings`, `feat:esi.corporation.standings`, `feat:quasar.standing`, `feat:social.contacts-add`, `feat:social.contacts-edit-delete`, `feat:social.contacts-block`, `feat:corporation.corp-contacts`, `feat:alliance.alliance-contacts`; `rpc:standingMgr.GetCharStandings`, `rpc:standingMgr.GetCorpStandings`, `rpc:standingMgr.GetNPCNPCStandings`, `rpc:standingMgr.GetStandingCompositions`, `rpc:standingMgr.GetStandingTransactions`; `notify:OnStandingSet`, `notify:OnStandingsModified`, `notify:OnSetPlayerStanding`, `notify:OnSetCorpStanding`, `notify:OnSetAllianceStanding`.

Auditor rows (`auditor_rows.json`): `feat:navigation.pochven-home-standing-gate`, `feat:pi.customs-office-tax-and-standings`, `feat:lifecycle.contacts-persist`.

Unit tests next to the sources in [`crates/eve-server/src/services/`](../../../crates/eve-server/src/services/): `agents_standings_arcs_tests.rs`, `agents_standings_arcs_edges_tests.rs`, `agents_standings_arcs_extra_unit_tests.rs`, `config_standings_bookmarks_deep_extra_unit_tests.rs`.

Integration tests in [`crates/eve-server/tests/it/`](../../../crates/eve-server/tests/it/): `agents_missions_complete.rs`, `pve_agent_missions_lifecycle.rs`, `rpc_lp_store_services.rs`, `pve_homefront_drifter_triglavian_live.rs`, `notify_fleet_corp_standings.rs`, `social_and_contacts_complete.rs`, `esi_characters_and_contacts.rs`.

---

## Related Systems & Cross-References

- [**Agent Dialogue, Missions, Standings & LP Stores**](../missions_and_agents/agent_dialogue_and_missions.md): gain and loss formulas, agent thresholds, mission penalties, LP stores.
- [**Skill Training & Medical Clones**](skill_training_and_medical_clones.md): the Social, Connections, Diplomacy and Criminal Connections skills.
- [**Crimewatch & Engagement Timers**](../combat_and_crimewatch/crimewatch_and_engagement_timers.md): security status, which is separate from standings.
- [**Faction Warfare & Insurgencies**](../sovereignty_and_faction_warfare/faction_warfare_and_insurgencies.md): enlistment and battlefield payouts.
- [**Pochven Navigation & Anchoring**](../universe_and_navigation/pochven_navigation_and_anchoring.md): Triglavian standing gates.
- [**Regional Market Order Matching**](../economy_and_markets/regional_market_order_matching.md): broker fees.
- [**NPC Combat AI & Ecology**](../npcs_and_living_world/npc_combat_ai_and_ecology.md): navy and EDENCOM responses.
- [**Mail & Chat**](../corporations_and_governance/mail_and_chat.md) and [**Fleets**](../corporations_and_governance/fleets.md): uses of contact standings.
- [**EvoMu Technical Wiki Table of Contents**](../README.md): master index and domain directory.

---

[← Back to Wiki Table of Contents](../README.md)
