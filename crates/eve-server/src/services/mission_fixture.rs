//! Test-only façade over mission persistence (`#1496` Track 1).
//!
//! Compiled under `#[cfg(any(test, feature = "testkit"))]` so both unit tests (`--lib`) and
//! integration tests (`--test it`) read, seed, mutate, and clear mission state without touching
//! `snap.active_missions`, `MissionTimingAndCargoMeta` owner notes, or `sp6_lifecycle` mission
//! wave maps directly.

#![forbid(unsafe_code)]

use crate::services::agents_missions::{
    load_mission_meta, save_mission_meta, MissionTimingAndCargoMeta,
};
use eve_db::{ActiveMissionRecord, WorldRepository, WorldStateSnapshot};

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

    /// Constructs a record-only mission fixture in the `"Offered"` state.
    pub fn offered(
        character_id: i64,
        mission_id: u32,
        agent_id: i64,
        solar_system_id: u32,
    ) -> Self {
        Self::new(character_id, mission_id, agent_id, solar_system_id, "Offered")
    }

    /// Constructs a record-only mission fixture in the `"Accepted"` state.
    pub fn accepted(
        character_id: i64,
        mission_id: u32,
        agent_id: i64,
        solar_system_id: u32,
    ) -> Self {
        Self::new(character_id, mission_id, agent_id, solar_system_id, "Accepted")
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

fn mission_wave_key(character_id: i64, mission_id: u32) -> String {
    format!("{character_id}:{mission_id}")
}

fn remove_mission_note(snap: &mut WorldStateSnapshot, character_id: i64, mission_id: u32) {
    let label = format!("mission_meta:{character_id}:{mission_id}");
    snap.owner_notes
        .retain(|_, note| !(note.character_id == -5 && note.label == label));
}

fn from_snapshot_parts(
    snap: &WorldStateSnapshot,
    key_mission_id: u32,
    rec: &ActiveMissionRecord,
) -> MissionFixture {
    let mkey = mission_wave_key(rec.character_id, rec.mission_id);
    let meta = load_mission_meta(snap, rec.character_id, rec.mission_id);
    let st = &snap.sp6_lifecycle;
    let awaiting_downtime_respawn = st
        .missions_awaiting_downtime_respawn
        .contains(&(rec.character_id, key_mission_id));
    let mut out = MissionFixture {
        character_id: rec.character_id,
        mission_id: rec.mission_id,
        agent_id: rec.agent_id,
        solar_system_id: rec.solar_system_id,
        state: rec.state.clone(),
        current_room_id: rec.current_room_id,
        npcs_remaining: rec.npcs_remaining,
        spawned_npc_ball_ids: rec.spawned_npc_ball_ids.clone(),
        bookmark_id: rec.bookmark_id,
        has_metadata: meta.is_some(),
        initial_wave_count: st.mission_initial_npc_counts.get(&mkey).copied(),
        initial_wave_types: st.mission_initial_npc_types.get(&mkey).cloned(),
        awaiting_downtime_respawn,
        ..Default::default()
    };
    if let Some(m) = meta {
        out.offered_at_unix = m.offered_at_unix;
        out.offer_expiry_unix = m.offer_expiry_unix;
        out.accepted_at_unix = m.accepted_at_unix;
        out.accept_expiry_unix = m.accept_expiry_unix;
        out.bonus_expiry_unix = m.bonus_expiry_unix;
        out.completed_at_unix = m.completed_at_unix;
        out.required_delivery_type_id = m.required_delivery_type_id;
        out.required_delivery_qty = m.required_delivery_qty;
        out.granted_courier_item_id = m.granted_courier_item_id;
        out.granted_courier_qty = m.granted_courier_qty;
        out.is_anomic_burner = m.is_anomic_burner;
        out.chosen_branch = m.chosen_branch;
        out.destination_station_id = m.destination_station_id;
        out.origin_station_id = m.origin_station_id;
        out.offered_level = m.offered_level;
    }
    out
}

/// Inserts or replaces a mission in `snap`, writing only the storage layers configured on `mission`.
pub fn insert(snap: &mut WorldStateSnapshot, mission: MissionFixture) {
    let key = (mission.character_id, mission.mission_id);
    snap.active_missions.insert(
        key,
        ActiveMissionRecord {
            mission_id: mission.mission_id,
            character_id: mission.character_id,
            agent_id: mission.agent_id,
            solar_system_id: mission.solar_system_id,
            state: mission.state.clone(),
            current_room_id: mission.current_room_id,
            npcs_remaining: mission.npcs_remaining,
            spawned_npc_ball_ids: mission.spawned_npc_ball_ids.clone(),
            bookmark_id: mission.bookmark_id,
        },
    );

    if mission.has_metadata || mission.has_any_non_default_metadata() {
        let meta = MissionTimingAndCargoMeta {
            mission_id: mission.mission_id,
            agent_id: mission.agent_id,
            offered_at_unix: mission.offered_at_unix,
            offer_expiry_unix: mission.offer_expiry_unix,
            accepted_at_unix: mission.accepted_at_unix,
            accept_expiry_unix: mission.accept_expiry_unix,
            bonus_expiry_unix: mission.bonus_expiry_unix,
            completed_at_unix: mission.completed_at_unix,
            required_delivery_type_id: mission.required_delivery_type_id,
            required_delivery_qty: mission.required_delivery_qty,
            granted_courier_item_id: mission.granted_courier_item_id,
            granted_courier_qty: mission.granted_courier_qty,
            is_anomic_burner: mission.is_anomic_burner,
            chosen_branch: mission.chosen_branch,
            destination_station_id: mission.destination_station_id,
            origin_station_id: mission.origin_station_id,
            offered_level: mission.offered_level,
        };
        save_mission_meta(snap, mission.character_id, &meta);
    } else {
        remove_mission_note(snap, mission.character_id, mission.mission_id);
    }

    let mkey = mission_wave_key(mission.character_id, mission.mission_id);
    let st = snap.sp6_lifecycle.as_inner_mut();
    match mission.initial_wave_count {
        Some(count) => {
            st.mission_initial_npc_counts.insert(mkey.clone(), count);
        }
        None => {
            st.mission_initial_npc_counts.remove(&mkey);
        }
    }
    match mission.initial_wave_types {
        Some(types) => {
            st.mission_initial_npc_types.insert(mkey, types);
        }
        None => {
            st.mission_initial_npc_types.remove(&mkey);
        }
    }
    if mission.awaiting_downtime_respawn {
        if !st.missions_awaiting_downtime_respawn.contains(&key) {
            st.missions_awaiting_downtime_respawn.push(key);
        }
    } else {
        st.missions_awaiting_downtime_respawn.retain(|&k| k != key);
    }
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
    snap.active_missions
        .get(&(character_id, mission_id))
        .map(|rec| from_snapshot_parts(snap, mission_id, rec))
}

/// Reads the mission identified by `(character_id, mission_id)` from `db`.
pub fn get_in(db: &WorldRepository, character_id: i64, mission_id: u32) -> Option<MissionFixture> {
    db.with_snapshot(|snap| get(snap, character_id, mission_id))
}

/// Reads `(character_id, mission_id)` from `snap`, passes it to `f`, and writes back the updated fixture.
pub fn edit<R>(
    snap: &mut WorldStateSnapshot,
    character_id: i64,
    mission_id: u32,
    f: impl FnOnce(&mut MissionFixture) -> R,
) -> Option<R> {
    let mut fixture = get(snap, character_id, mission_id)?;
    let out = f(&mut fixture);
    if fixture.character_id != character_id || fixture.mission_id != mission_id {
        remove(snap, character_id, mission_id);
    }
    insert(snap, fixture);
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

/// Removes `(character_id, mission_id)` and all associated note/wave/respawn entries from `snap`.
pub fn remove(snap: &mut WorldStateSnapshot, character_id: i64, mission_id: u32) -> bool {
    let key = (character_id, mission_id);
    let existed = snap.active_missions.remove(&key).is_some();
    remove_mission_note(snap, character_id, mission_id);
    let mkey = mission_wave_key(character_id, mission_id);
    let st = snap.sp6_lifecycle.as_inner_mut();
    st.mission_initial_npc_counts.remove(&mkey);
    st.mission_initial_npc_types.remove(&mkey);
    st.missions_awaiting_downtime_respawn.retain(|&k| k != key);
    existed
}

/// Removes `(character_id, mission_id)` and all associated entries from `db`.
pub fn remove_in(db: &WorldRepository, character_id: i64, mission_id: u32) -> bool {
    db.with_mut_snapshot(|snap| remove(snap, character_id, mission_id))
}

/// Clears all missions and associated note/wave/respawn entries belonging to `character_id` from `snap`.
pub fn clear_character(snap: &mut WorldStateSnapshot, character_id: i64) {
    snap.active_missions
        .retain(|(cid, _), _| *cid != character_id);
    let note_prefix = format!("mission_meta:{character_id}:");
    snap.owner_notes
        .retain(|_, note| !(note.character_id == -5 && note.label.starts_with(&note_prefix)));
    let wave_prefix = format!("{character_id}:");
    let st = snap.sp6_lifecycle.as_inner_mut();
    st.mission_initial_npc_counts
        .retain(|k, _| !k.starts_with(&wave_prefix));
    st.mission_initial_npc_types
        .retain(|k, _| !k.starts_with(&wave_prefix));
    st.missions_awaiting_downtime_respawn
        .retain(|&(cid, _)| cid != character_id);
}

/// Clears all missions and associated entries belonging to `character_id` from `db`.
pub fn clear_character_in(db: &WorldRepository, character_id: i64) {
    db.with_mut_snapshot(|snap| clear_character(snap, character_id));
}

/// Clears all missions and associated note/wave/respawn entries across all characters from `snap`.
pub fn clear_all(snap: &mut WorldStateSnapshot) {
    snap.active_missions.clear();
    snap.owner_notes
        .retain(|_, note| !(note.character_id == -5 && note.label.starts_with("mission_meta:")));
    let st = snap.sp6_lifecycle.as_inner_mut();
    st.mission_initial_npc_counts.clear();
    st.mission_initial_npc_types.clear();
    st.missions_awaiting_downtime_respawn.clear();
}

/// Clears all missions and associated entries across all characters from `db`.
pub fn clear_all_in(db: &WorldRepository) {
    db.with_mut_snapshot(clear_all);
}

/// Lists all missions in `snap`, sorted deterministically by `(character_id, mission_id)`.
pub fn list(snap: &WorldStateSnapshot) -> Vec<MissionFixture> {
    let mut out: Vec<MissionFixture> = snap
        .active_missions
        .iter()
        .map(|(&(_, key_mid), rec)| from_snapshot_parts(snap, key_mid, rec))
        .collect();
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
        .active_missions
        .iter()
        .filter(|(_, rec)| rec.character_id == character_id)
        .map(|(&(_, key_mid), rec)| from_snapshot_parts(snap, key_mid, rec))
        .collect();
    out.sort_by_key(|m| m.mission_id);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_full_mission_fixture_preserves_every_field() {
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
            .completed_at_unix(Some(1_700_002_500))
            .required_delivery_type_id(Some(34))
            .required_delivery_qty(150)
            .granted_courier_item_id(Some(88_001))
            .granted_courier_qty(20)
            .is_anomic_burner(true)
            .chosen_branch(Some("AmarrBranch".into()))
            .destination_station_id(Some(60_003_760))
            .origin_station_id(Some(60_003_763))
            .offered_level(Some(4))
            .initial_wave_count(Some(7))
            .initial_wave_types(Some(vec![10_019, 10_020]))
            .awaiting_downtime_respawn(true);

        insert(&mut snap, full.clone());
        let got = get(&snap, 90_000_001, 42_001).expect("inserted mission must exist");
        assert_eq!(got, full);
        assert!(load_mission_meta(&snap, 90_000_001, 42_001).is_some());
        assert_eq!(
            snap.sp6_lifecycle
                .mission_initial_npc_counts
                .get("90000001:42001"),
            Some(&7)
        );
        assert_eq!(
            snap.sp6_lifecycle
                .mission_initial_npc_types
                .get("90000001:42001"),
            Some(&vec![10_019, 10_020])
        );
        assert!(snap
            .sp6_lifecycle
            .missions_awaiting_downtime_respawn
            .contains(&(90_000_001, 42_001)));
    }

    #[test]
    fn roundtrip_record_only_mission_writes_no_note_or_sp6_rows() {
        let mut snap = WorldStateSnapshot::default();
        let record_only = MissionFixture::offered(90_000_002, 13_512, 3_012_050, 30_000_144)
            .current_room_id(1)
            .npcs_remaining(3)
            .spawned_npc_ball_ids(vec![911_000_001])
            .bookmark_id(Some(88_123));

        insert(&mut snap, record_only.clone());
        let got = get(&snap, 90_000_002, 13_512).expect("record-only mission must exist");
        assert_eq!(got, record_only);
        assert!(!got.has_metadata);
        assert!(load_mission_meta(&snap, 90_000_002, 13_512).is_none());
        assert!(snap.owner_notes.is_empty());
        assert!(snap.sp6_lifecycle.mission_initial_npc_counts.is_empty());
        assert!(snap.sp6_lifecycle.mission_initial_npc_types.is_empty());
        assert!(snap
            .sp6_lifecycle
            .missions_awaiting_downtime_respawn
            .is_empty());
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

        // Editing record-only keeps it record-only; editing metadata preserves metadata.
        assert!(edit(&mut snap, 90_000_001, 100, |m| {
            m.state = "Failed".into();
            m.npcs_remaining = 0;
        })
        .is_some());
        let failed = get(&snap, 90_000_001, 100).unwrap();
        assert_eq!(failed.state, "Failed");
        assert!(!failed.has_metadata);
        assert!(load_mission_meta(&snap, 90_000_001, 100).is_none());

        assert!(edit(&mut snap, 90_000_001, 300, |m| {
            m.completed_at_unix = Some(1_700_005_000);
            m.awaiting_downtime_respawn = true;
        })
        .is_some());
        let updated = get(&snap, 90_000_001, 300).unwrap();
        assert_eq!(updated.completed_at_unix, Some(1_700_005_000));
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
