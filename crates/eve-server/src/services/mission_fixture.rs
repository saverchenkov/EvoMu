//! Test-only façade over mission persistence (`#1496` Track 1 / Track 3).
//!
//! Compiled under `#[cfg(any(test, feature = "testkit"))]` so both unit tests (`--lib`) and
//! integration tests (`--test it`) read, seed, mutate, and clear mission state without touching
//! `MissionInstanceStore` directly.

#![forbid(unsafe_code)]

use eve_db::{
    decide_mission_kind, lookup_sde_mission_and_agent, open_sde_sqlite_ro, CourierObjective,
    DeliveryObjective, EncounterObjective, MissionHistoryEntry, MissionInstance, MissionKind,
    MissionObjective, MissionState, WorldRepository, WorldStateSnapshot,
};

/// Plain test value representing one `(character_id, mission_id)` mission entry across the
/// active mission table, optional timing/cargo metadata note, and `sp6_lifecycle` wave state.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MissionFixture {
    pub character_id: i64,
    pub mission_id: u32,
    pub agent_id: i64,
    pub solar_system_id: u32,
    pub state: String,
    pub current_room_id: u32,
    pub npcs_remaining: u32,
    pub spawned_npc_ball_ids: Vec<i64>,
    pub bookmark_id: Option<i64>,

    // Metadata fields backed by `MissionTimingAndCargoMeta` when `has_metadata` is true.
    pub has_metadata: bool,
    pub offered_at_unix: u64,
    pub offer_expiry_unix: u64,
    pub accepted_at_unix: Option<u64>,
    pub accept_expiry_unix: Option<u64>,
    pub bonus_expiry_unix: Option<u64>,
    pub completed_at_unix: Option<u64>,
    pub required_delivery_type_id: Option<u32>,
    pub required_delivery_qty: i32,
    pub granted_courier_item_id: Option<i64>,
    pub granted_courier_qty: i32,
    pub is_anomic_burner: bool,
    pub chosen_branch: Option<String>,
    pub destination_station_id: Option<i64>,
    pub origin_station_id: Option<i64>,
    pub offered_level: Option<u8>,

    // Initial wave / downtime respawn state backed by `sp6_lifecycle`.
    pub initial_wave_count: Option<u32>,
    pub initial_wave_types: Option<Vec<u32>>,
    pub awaiting_downtime_respawn: bool,
}

impl MissionFixture {
    /// Constructs a record-only mission fixture with explicit `state`.
    pub fn new(
        character_id: i64,
        mission_id: u32,
        agent_id: i64,
        solar_system_id: u32,
        state: impl Into<String>,
    ) -> Self {
        Self {
            character_id,
            mission_id,
            agent_id,
            solar_system_id,
            state: state.into(),
            ..Default::default()
        }
    }

    /// Constructs a record-only mission fixture in the `Offered` state.
    pub fn offered(
        character_id: i64,
        mission_id: u32,
        agent_id: i64,
        solar_system_id: u32,
    ) -> Self {
        Self::new(
            character_id,
            mission_id,
            agent_id,
            solar_system_id,
            MissionState::Offered.as_str(),
        )
    }

    /// Constructs a record-only mission fixture in the `Accepted` state.
    pub fn accepted(
        character_id: i64,
        mission_id: u32,
        agent_id: i64,
        solar_system_id: u32,
    ) -> Self {
        Self::new(
            character_id,
            mission_id,
            agent_id,
            solar_system_id,
            MissionState::Accepted.as_str(),
        )
    }

    /// Sets the mission state string (e.g. `"Offered"`, `"Accepted"`, `"Completed"`, `"Expired"`, `"Failed"`).
    pub fn state(mut self, state: impl Into<String>) -> Self {
        self.state = state.into();
        self
    }

    /// Sets the offering agent ID.
    pub fn agent_id(mut self, agent_id: i64) -> Self {
        self.agent_id = agent_id;
        self
    }

    /// Sets the mission solar system ID.
    pub fn solar_system_id(self, solar_system_id: u32) -> Self {
        Self { solar_system_id, ..self }
    }

    /// Sets the current dungeon room ID.
    pub fn current_room_id(mut self, current_room_id: u32) -> Self {
        self.current_room_id = current_room_id;
        self
    }

    /// Sets the remaining NPC count for the current room.
    pub fn npcs_remaining(mut self, npcs_remaining: u32) -> Self {
        self.npcs_remaining = npcs_remaining;
        self
    }

    /// Sets the currently spawned mission NPC ball IDs.
    pub fn spawned_npc_ball_ids(mut self, spawned_npc_ball_ids: Vec<i64>) -> Self {
        self.spawned_npc_ball_ids = spawned_npc_ball_ids;
        self
    }

    /// Sets the associated mission pocket bookmark ID.
    pub fn bookmark_id(mut self, bookmark_id: Option<i64>) -> Self {
        self.bookmark_id = bookmark_id;
        self
    }

    /// Marks the fixture as having a persisted metadata record even if all note fields are default.
    pub fn with_metadata(mut self) -> Self {
        self.has_metadata = true;
        self
    }

    /// Sets `offered_at_unix` and enables metadata persistence.
    pub fn offered_at_unix(mut self, offered_at_unix: u64) -> Self {
        self.has_metadata = true;
        self.offered_at_unix = offered_at_unix;
        self
    }

    /// Sets `offer_expiry_unix` and enables metadata persistence.
    pub fn offer_expiry_unix(mut self, offer_expiry_unix: u64) -> Self {
        self.has_metadata = true;
        self.offer_expiry_unix = offer_expiry_unix;
        self
    }

    /// Sets `accepted_at_unix` and enables metadata persistence.
    pub fn accepted_at_unix(mut self, accepted_at_unix: Option<u64>) -> Self {
        self.has_metadata = true;
        self.accepted_at_unix = accepted_at_unix;
        self
    }

    /// Sets `accept_expiry_unix` and enables metadata persistence.
    pub fn accept_expiry_unix(mut self, accept_expiry_unix: Option<u64>) -> Self {
        self.has_metadata = true;
        self.accept_expiry_unix = accept_expiry_unix;
        self
    }

    /// Sets `bonus_expiry_unix` and enables metadata persistence.
    pub fn bonus_expiry_unix(mut self, bonus_expiry_unix: Option<u64>) -> Self {
        self.has_metadata = true;
        self.bonus_expiry_unix = bonus_expiry_unix;
        self
    }

    /// Sets `completed_at_unix` and enables metadata persistence.
    pub fn completed_at_unix(mut self, completed_at_unix: Option<u64>) -> Self {
        self.has_metadata = true;
        self.completed_at_unix = completed_at_unix;
        self
    }

    /// Sets `required_delivery_type_id` and enables metadata persistence.
    pub fn required_delivery_type_id(mut self, required_delivery_type_id: Option<u32>) -> Self {
        self.has_metadata = true;
        self.required_delivery_type_id = required_delivery_type_id;
        self
    }

    /// Sets `required_delivery_qty` and enables metadata persistence.
    pub fn required_delivery_qty(mut self, required_delivery_qty: i32) -> Self {
        self.has_metadata = true;
        self.required_delivery_qty = required_delivery_qty;
        self
    }

    /// Sets `granted_courier_item_id` and enables metadata persistence.
    pub fn granted_courier_item_id(mut self, granted_courier_item_id: Option<i64>) -> Self {
        self.has_metadata = true;
        self.granted_courier_item_id = granted_courier_item_id;
        self
    }

    /// Sets `granted_courier_qty` and enables metadata persistence.
    pub fn granted_courier_qty(mut self, granted_courier_qty: i32) -> Self {
        self.has_metadata = true;
        self.granted_courier_qty = granted_courier_qty;
        self
    }

    /// Sets `is_anomic_burner` and enables metadata persistence.
    pub fn is_anomic_burner(mut self, is_anomic_burner: bool) -> Self {
        self.has_metadata = true;
        self.is_anomic_burner = is_anomic_burner;
        self
    }

    /// Sets `chosen_branch` and enables metadata persistence.
    pub fn chosen_branch(mut self, chosen_branch: Option<String>) -> Self {
        self.has_metadata = true;
        self.chosen_branch = chosen_branch;
        self
    }

    /// Sets `destination_station_id` and enables metadata persistence.
    pub fn destination_station_id(mut self, destination_station_id: Option<i64>) -> Self {
        self.has_metadata = true;
        self.destination_station_id = destination_station_id;
        self
    }

    /// Sets `origin_station_id` and enables metadata persistence.
    pub fn origin_station_id(mut self, origin_station_id: Option<i64>) -> Self {
        self.has_metadata = true;
        self.origin_station_id = origin_station_id;
        self
    }

    /// Sets `offered_level` and enables metadata persistence.
    pub fn offered_level(mut self, offered_level: Option<u8>) -> Self {
        self.has_metadata = true;
        self.offered_level = offered_level;
        self
    }

    /// Sets the `sp6_lifecycle` initial wave NPC count (`None` clears it).
    pub fn initial_wave_count(mut self, initial_wave_count: Option<u32>) -> Self {
        self.initial_wave_count = initial_wave_count;
        self
    }

    /// Sets the `sp6_lifecycle` initial wave NPC type list (`None` clears it).
    pub fn initial_wave_types(mut self, initial_wave_types: Option<Vec<u32>>) -> Self {
        self.initial_wave_types = initial_wave_types;
        self
    }

    /// Sets whether `(character_id, mission_id)` is marked awaiting downtime respawn.
    pub fn awaiting_downtime_respawn(mut self, awaiting_downtime_respawn: bool) -> Self {
        self.awaiting_downtime_respawn = awaiting_downtime_respawn;
        self
    }

    fn has_any_non_default_metadata(&self) -> bool {
        self.offered_at_unix != 0
            || self.offer_expiry_unix != 0
            || self.accepted_at_unix.is_some()
            || self.accept_expiry_unix.is_some()
            || self.bonus_expiry_unix.is_some()
            || self.completed_at_unix.is_some()
            || self.required_delivery_type_id.is_some()
            || self.required_delivery_qty != 0
            || self.granted_courier_item_id.is_some()
            || self.granted_courier_qty != 0
            || self.is_anomic_burner
            || self.chosen_branch.is_some()
            || self.destination_station_id.is_some()
            || self.origin_station_id.is_some()
            || self.offered_level.is_some()
    }
}

fn has_cargo_or_courier_fields(mission: &MissionFixture) -> bool {
    mission.required_delivery_type_id.is_some()
        || mission.required_delivery_qty != 0
        || mission.destination_station_id.is_some()
        || mission.granted_courier_item_id.is_some()
        || mission.granted_courier_qty != 0
}

fn infer_kind_with_sde(
    mission: &MissionFixture,
    sde: Option<&eve_sde::SdeRegistry>,
) -> MissionKind {
    let (tmpl, agent) = if let Some(sde) = sde {
        (
            sde.missions.get(&mission.mission_id).cloned(),
            sde.agents.get(&mission.agent_id).cloned(),
        )
    } else {
        open_sde_sqlite_ro()
            .map(|conn| lookup_sde_mission_and_agent(&conn, mission.mission_id, mission.agent_id))
            .unwrap_or((None, None))
    };
    let base_kind = decide_mission_kind(tmpl.as_ref(), agent.as_ref());
    let has_courier_fields = mission.granted_courier_item_id.is_some()
        || mission.granted_courier_qty != 0
        || mission.destination_station_id.is_some();
    let has_delivery_fields =
        mission.required_delivery_type_id.is_some() || mission.required_delivery_qty != 0;
    let has_encounter_fields = mission.npcs_remaining != 0
        || !mission.spawned_npc_ball_ids.is_empty()
        || mission.initial_wave_count.is_some()
        || mission.initial_wave_types.is_some()
        || mission.awaiting_downtime_respawn;

    if has_courier_fields {
        return MissionKind::Courier;
    }
    if has_encounter_fields && !has_delivery_fields {
        return MissionKind::Encounter;
    }
    match base_kind {
        MissionKind::ResearchTurnIn
        | MissionKind::DataCenterTurnIn
        | MissionKind::Mining
        | MissionKind::Trade
        | MissionKind::Courier
        | MissionKind::Heraldry => base_kind,
        MissionKind::Encounter => {
            if has_delivery_fields {
                MissionKind::Trade
            } else {
                MissionKind::Encounter
            }
        }
    }
}

fn infer_kind(mission: &MissionFixture) -> MissionKind {
    infer_kind_with_sde(mission, None)
}

fn from_instance(inst: &MissionInstance) -> MissionFixture {
    let mut out = MissionFixture {
        character_id: inst.character_id,
        mission_id: inst.template_id,
        agent_id: inst.agent_id,
        solar_system_id: inst.solar_system_id,
        state: inst.state.as_str().to_string(),
        current_room_id: inst.current_room_id,
        bookmark_id: inst.bookmark_id,
        offered_at_unix: inst.offered_at_unix,
        offer_expiry_unix: inst.offer_expiry_unix,
        accepted_at_unix: inst.accepted_at_unix,
        accept_expiry_unix: inst.accept_expiry_unix,
        bonus_expiry_unix: inst.bonus_expiry_unix,
        completed_at_unix: None,
        is_anomic_burner: inst.is_anomic_burner,
        chosen_branch: None,
        origin_station_id: inst.origin_station_id,
        offered_level: inst.offered_level,
        ..Default::default()
    };
    match &inst.objective {
        MissionObjective::Encounter(enc) => {
            out.npcs_remaining = enc.npcs_remaining;
            out.spawned_npc_ball_ids = enc.spawned_npc_ball_ids.clone();
            out.initial_wave_count = enc.initial_wave_count;
            out.initial_wave_types = if enc.initial_wave_types.is_empty() {
                None
            } else {
                Some(enc.initial_wave_types.clone())
            };
            out.awaiting_downtime_respawn = enc.awaiting_downtime_respawn;
        }
        MissionObjective::Courier(c) => {
            out.required_delivery_type_id = c.cargo_type_id;
            out.required_delivery_qty = c.quantity.max(0);
            out.destination_station_id = c.destination_station_id;
            out.granted_courier_item_id = c.granted_item_id;
            out.granted_courier_qty = c.granted_qty.max(0);
        }
        MissionObjective::Delivery(d) => {
            out.required_delivery_type_id = d.item_type_id;
            out.required_delivery_qty = d.quantity.max(0);
        }
        MissionObjective::None => {}
    }
    out.has_metadata = out.has_any_non_default_metadata();
    out
}

fn from_history_entry_fixture(entry: &MissionHistoryEntry) -> MissionFixture {
    MissionFixture {
        character_id: entry.character_id,
        mission_id: entry.template_id,
        agent_id: entry.agent_id,
        solar_system_id: 0,
        state: entry.final_state.as_str().to_string(),
        current_room_id: 1,
        offered_at_unix: entry.offered_at_unix,
        completed_at_unix: Some(entry.ended_at_unix),
        has_metadata: true,
        ..Default::default()
    }
}

fn insert_with_kind(
    snap: &mut WorldStateSnapshot,
    mission: MissionFixture,
    kind: MissionKind,
    existing_instance_id: Option<i64>,
) {
    let instance_id = existing_instance_id.unwrap_or_else(|| snap.alloc_entity_id());
    let state = MissionState::parse(&mission.state).unwrap_or_else(|| {
        panic!(
            "invalid MissionFixture state {:?} for ({}, {})",
            mission.state, mission.character_id, mission.mission_id
        )
    });
    if !state.is_active() {
        snap.missions
            .remove_by_character_template(mission.character_id, mission.mission_id);
        snap.missions.remove_history(instance_id);
        let ended_at_unix = mission
            .completed_at_unix
            .or(mission.accepted_at_unix)
            .unwrap_or(mission.offered_at_unix);
        snap.missions
            .record_history(MissionHistoryEntry {
                instance_id,
                character_id: mission.character_id,
                agent_id: mission.agent_id,
                template_id: mission.mission_id,
                kind,
                final_state: state,
                offered_at_unix: mission.offered_at_unix,
                ended_at_unix,
            })
            .expect("record mission history fixture");
        return;
    }
    let objective = match kind {
        MissionKind::Encounter => MissionObjective::Encounter(EncounterObjective {
            npcs_remaining: mission.npcs_remaining,
            spawned_npc_ball_ids: mission.spawned_npc_ball_ids,
            initial_wave_count: mission.initial_wave_count,
            initial_wave_types: mission.initial_wave_types.unwrap_or_default(),
            awaiting_downtime_respawn: mission.awaiting_downtime_respawn,
        }),
        MissionKind::Courier => MissionObjective::Courier(CourierObjective {
            cargo_type_id: mission.required_delivery_type_id,
            quantity: i32::try_from(mission.required_delivery_qty).unwrap_or(i32::MAX),
            destination_station_id: mission.destination_station_id,
            granted_item_id: mission.granted_courier_item_id,
            granted_qty: i32::try_from(mission.granted_courier_qty).unwrap_or(i32::MAX),
        }),
        MissionKind::Trade
        | MissionKind::Mining
        | MissionKind::Heraldry
        | MissionKind::DataCenterTurnIn
        | MissionKind::ResearchTurnIn => MissionObjective::Delivery(DeliveryObjective {
            item_type_id: mission.required_delivery_type_id,
            quantity: i32::try_from(mission.required_delivery_qty).unwrap_or(i32::MAX),
        }),
    };
    snap.missions
        .upsert_by_character_template(MissionInstance {
            instance_id,
            character_id: mission.character_id,
            agent_id: mission.agent_id,
            template_id: mission.mission_id,
            state,
            kind,
            solar_system_id: mission.solar_system_id,
            current_room_id: mission.current_room_id,
            bookmark_id: mission.bookmark_id,
            origin_station_id: mission.origin_station_id,
            offered_level: mission.offered_level,
            is_anomic_burner: mission.is_anomic_burner,
            offered_at_unix: mission.offered_at_unix,
            offer_expiry_unix: mission.offer_expiry_unix,
            accepted_at_unix: mission.accepted_at_unix,
            accept_expiry_unix: mission.accept_expiry_unix,
            bonus_expiry_unix: mission.bonus_expiry_unix,
            objective,
        })
        .expect("upsert mission fixture");
}

/// Inserts or replaces a mission in `snap`.
pub fn insert(snap: &mut WorldStateSnapshot, mission: MissionFixture) {
    let existing_id = snap
        .missions
        .get_by_character_template(mission.character_id, mission.mission_id)
        .map(|m| m.instance_id);
    let kind = infer_kind(&mission);
    insert_with_kind(snap, mission, kind, existing_id);
}

/// Inserts or replaces a mission in `snap`, resolving template/agent classification from `sde`.
pub fn insert_with_sde(
    snap: &mut WorldStateSnapshot,
    sde: &eve_sde::SdeRegistry,
    mission: MissionFixture,
) {
    let existing_id = snap
        .missions
        .get_by_character_template(mission.character_id, mission.mission_id)
        .map(|m| m.instance_id);
    let kind = infer_kind_with_sde(&mission, Some(sde));
    insert_with_kind(snap, mission, kind, existing_id);
}

/// Inserts or replaces a mission in `db` inside a snapshot write transaction.
pub fn insert_in(db: &WorldRepository, mission: MissionFixture) {
    db.with_mut_snapshot(|snap| insert(snap, mission));
}

/// Reads the mission identified by `(character_id, mission_id)` from `snap`.
pub fn get(
    snap: &WorldStateSnapshot,
    character_id: i64,
    mission_id: u32,
) -> Option<MissionFixture> {
    snap.missions
        .get_by_character_template(character_id, mission_id)
        .map(from_instance)
}

/// Reads the mission identified by `(character_id, mission_id)` from `db`.
pub fn get_in(db: &WorldRepository, character_id: i64, mission_id: u32) -> Option<MissionFixture> {
    db.with_snapshot(|snap| get(snap, character_id, mission_id))
}

/// Reads `(character_id, mission_id)` from `snap` (falling back to latest history), passes it to `f`, and writes back the updated fixture.
pub fn edit<R>(
    snap: &mut WorldStateSnapshot,
    character_id: i64,
    mission_id: u32,
    f: impl FnOnce(&mut MissionFixture) -> R,
) -> Option<R> {
    if let Some(existing) = snap
        .missions
        .get_by_character_template(character_id, mission_id)
    {
        let existing_id = existing.instance_id;
        let existing_kind = existing.kind;
        let mut fixture = from_instance(existing);
        let prev_had_cargo = has_cargo_or_courier_fields(&fixture);
        let out = f(&mut fixture);
        let now_has_cargo = has_cargo_or_courier_fields(&fixture);
        let next_kind = if prev_had_cargo && !now_has_cargo {
            MissionKind::Encounter
        } else if !prev_had_cargo && now_has_cargo {
            infer_kind(&fixture)
        } else {
            existing_kind
        };
        if fixture.character_id != character_id || fixture.mission_id != mission_id {
            snap.missions
                .remove_by_character_template(character_id, mission_id);
        }
        insert_with_kind(snap, fixture, next_kind, Some(existing_id));
        return Some(out);
    }

    let hist = snap
        .missions
        .latest_history_for_character_template(character_id, mission_id)?
        .clone();
    let mut fixture = from_history_entry_fixture(&hist);
    let out = f(&mut fixture);
    snap.missions.remove_history(hist.instance_id);
    let next_kind = if has_cargo_or_courier_fields(&fixture) {
        infer_kind(&fixture)
    } else {
        hist.kind
    };
    insert_with_kind(snap, fixture, next_kind, Some(hist.instance_id));
    Some(out)
}

/// Reads `(character_id, mission_id)` from `db`, passes it to `f`, and writes back the updated fixture.
pub fn edit_in<R>(
    db: &WorldRepository,
    character_id: i64,
    mission_id: u32,
    f: impl FnOnce(&mut MissionFixture) -> R,
) -> Option<R> {
    db.with_mut_snapshot(|snap| edit(snap, character_id, mission_id, f))
}

/// Removes `(character_id, mission_id)` from active missions and archived history in `snap`.
pub fn remove(snap: &mut WorldStateSnapshot, character_id: i64, mission_id: u32) -> bool {
    let active = snap
        .missions
        .remove_by_character_template(character_id, mission_id)
        .is_some();
    let hist = snap
        .missions
        .remove_history_for_character_template(character_id, mission_id)
        > 0;
    active || hist
}

/// Removes `(character_id, mission_id)` from `db`.
pub fn remove_in(db: &WorldRepository, character_id: i64, mission_id: u32) -> bool {
    db.with_mut_snapshot(|snap| remove(snap, character_id, mission_id))
}

/// Clears all active missions and archived history belonging to `character_id` from `snap`.
pub fn clear_character(snap: &mut WorldStateSnapshot, character_id: i64) {
    let mids: Vec<u32> = snap
        .missions
        .iter_by_character(character_id)
        .map(|m| m.template_id)
        .collect();
    for mid in mids {
        snap.missions.remove_by_character_template(character_id, mid);
    }
    snap.missions.remove_history_by_character(character_id);
}

/// Clears all missions belonging to `character_id` from `db`.
pub fn clear_character_in(db: &WorldRepository, character_id: i64) {
    db.with_mut_snapshot(|snap| clear_character(snap, character_id));
}

/// Clears all missions across all characters from `snap`.
pub fn clear_all(snap: &mut WorldStateSnapshot) {
    snap.missions.clear();
}

/// Clears all missions across all characters from `db`.
pub fn clear_all_in(db: &WorldRepository) {
    db.with_mut_snapshot(clear_all);
}

/// Lists all missions in `snap`, sorted deterministically by `(character_id, mission_id)`.
pub fn list(snap: &WorldStateSnapshot) -> Vec<MissionFixture> {
    let mut out: Vec<MissionFixture> = snap.missions.iter().map(from_instance).collect();
    out.sort_by_key(|m| (m.character_id, m.mission_id));
    out
}

/// Lists all missions in `db`, sorted deterministically by `(character_id, mission_id)`.
pub fn list_in(db: &WorldRepository) -> Vec<MissionFixture> {
    db.with_snapshot(list)
}

/// Lists all missions for `character_id` in `snap`, sorted deterministically by `mission_id`.
pub fn list_for_character(snap: &WorldStateSnapshot, character_id: i64) -> Vec<MissionFixture> {
    let mut out: Vec<MissionFixture> = snap
        .missions
        .iter_by_character(character_id)
        .map(from_instance)
        .collect();
    out.sort_by_key(|m| m.mission_id);
    out
}

/// Returns the latest archived terminal state string for `(character_id, mission_id)`.
pub fn latest_history(
    snap: &WorldStateSnapshot,
    character_id: i64,
    mission_id: u32,
) -> Option<String> {
    snap.missions
        .latest_history_for_character_template(character_id, mission_id)
        .map(|e| e.final_state.as_str().to_string())
}

/// Lists all archived mission history entries for `character_id`, ordered chronologically.
pub fn list_history_for_character(
    snap: &WorldStateSnapshot,
    character_id: i64,
) -> Vec<MissionFixture> {
    let mut entries: Vec<&MissionHistoryEntry> = snap
        .missions
        .iter_history_by_character(character_id)
        .collect();
    entries.sort_by_key(|e| (e.ended_at_unix, e.instance_id));
    entries
        .into_iter()
        .map(from_history_entry_fixture)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_full_encounter_mission_fixture_preserves_encounter_fields() {
        let mut snap = WorldStateSnapshot::default();
        let full = MissionFixture::accepted(90_000_001, 42_001, 3_018_418, 30_000_142)
            .current_room_id(2)
            .npcs_remaining(5)
            .spawned_npc_ball_ids(vec![910_000_001, 910_000_002])
            .bookmark_id(Some(77_001))
            .offered_at_unix(1_700_000_000)
            .offer_expiry_unix(1_700_086_400)
            .accepted_at_unix(Some(1_700_000_100))
            .accept_expiry_unix(Some(1_700_604_800))
            .bonus_expiry_unix(Some(1_700_003_700))
            .is_anomic_burner(true)
            .origin_station_id(Some(60_003_763))
            .offered_level(Some(4))
            .initial_wave_count(Some(7))
            .initial_wave_types(Some(vec![10_019, 10_020]))
            .awaiting_downtime_respawn(true);

        insert(&mut snap, full.clone());
        let got = get(&snap, 90_000_001, 42_001).expect("inserted mission must exist");
        assert_eq!(got, full);
        assert!(snap.owner_notes.is_empty());
        let stored = snap
            .missions
            .get_by_character_template(90_000_001, 42_001)
            .expect("instance stored");
        assert_eq!(stored.kind, MissionKind::Encounter);
        assert_eq!(stored.initial_wave_count(), Some(7));
        assert_eq!(stored.initial_wave_types(), &[10_019, 10_020]);
        assert!(stored.awaiting_downtime_respawn());
        assert!(snap.missions.contains_spawned_npc_id(910_000_001));
    }

    #[test]
    fn roundtrip_full_courier_and_minimal_offered_mission_fixtures() {
        let mut snap = WorldStateSnapshot::default();
        let courier = MissionFixture::accepted(90_000_002, 42_002, 3_012_050, 30_000_144)
            .bookmark_id(Some(88_123))
            .offered_at_unix(1_700_000_000)
            .offer_expiry_unix(1_700_086_400)
            .accepted_at_unix(Some(1_700_000_100))
            .required_delivery_type_id(Some(34))
            .required_delivery_qty(150)
            .granted_courier_item_id(Some(88_001))
            .granted_courier_qty(20)
            .destination_station_id(Some(60_003_760))
            .origin_station_id(Some(60_003_763))
            .offered_level(Some(2));
        insert(&mut snap, courier.clone());
        let got_courier = get(&snap, 90_000_002, 42_002).expect("courier mission must exist");
        assert_eq!(got_courier, courier);
        assert_eq!(
            snap.missions
                .get_by_character_template(90_000_002, 42_002)
                .map(|m| m.kind),
            Some(MissionKind::Courier)
        );

        let record_only = MissionFixture::offered(90_000_002, 13_512, 3_012_050, 30_000_144)
            .current_room_id(1)
            .npcs_remaining(3)
            .spawned_npc_ball_ids(vec![911_000_001])
            .bookmark_id(Some(88_124));
        insert(&mut snap, record_only.clone());
        let got = get(&snap, 90_000_002, 13_512).expect("minimal mission must exist");
        assert_eq!(got, record_only);
        assert!(!got.has_metadata);
        assert!(snap.owner_notes.is_empty());
    }

    #[test]
    fn edit_remove_clear_character_clear_all_and_sorted_list_roundtrip() {
        let mut snap = WorldStateSnapshot::default();
        insert(
            &mut snap,
            MissionFixture::offered(90_000_002, 200, 3_000_002, 30_000_142),
        );
        insert(
            &mut snap,
            MissionFixture::accepted(90_000_001, 300, 3_000_001, 30_000_142)
                .offered_level(Some(2)),
        );
        insert(
            &mut snap,
            MissionFixture::offered(90_000_001, 100, 3_000_001, 30_000_142),
        );

        let listed: Vec<(i64, u32)> = list(&snap)
            .into_iter()
            .map(|m| (m.character_id, m.mission_id))
            .collect();
        assert_eq!(
            listed,
            vec![(90_000_001, 100), (90_000_001, 300), (90_000_002, 200)]
        );

        assert!(edit(&mut snap, 90_000_001, 100, |m| {
            m.state = "Failed".into();
            m.npcs_remaining = 0;
        })
        .is_some());
        assert!(get(&snap, 90_000_001, 100).is_none());
        assert_eq!(
            latest_history(&snap, 90_000_001, 100).as_deref(),
            Some("Failed")
        );
        assert_eq!(list_history_for_character(&snap, 90_000_001).len(), 1);

        assert!(edit(&mut snap, 90_000_001, 300, |m| {
            m.awaiting_downtime_respawn = true;
        })
        .is_some());
        let updated = get(&snap, 90_000_001, 300).unwrap();
        assert_eq!(updated.offered_level, Some(2));
        assert!(updated.awaiting_downtime_respawn);

        assert!(remove(&mut snap, 90_000_001, 100));
        assert!(get(&snap, 90_000_001, 100).is_none());

        clear_character(&mut snap, 90_000_001);
        assert_eq!(list_for_character(&snap, 90_000_001).len(), 0);
        assert_eq!(list(&snap).len(), 1);

        clear_all(&mut snap);
        assert!(list(&snap).is_empty());
        assert!(snap.owner_notes.is_empty());
    }
}
