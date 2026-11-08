//! Extra unit and mutation-killing tests for `structure_control_ops`.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::station_structure::structure_upwell_ops::{
    load_ext_state, save_ext_state, AccessGroupRecord, StructureAccessProfile,
    StructureLifecycleState,
};
use crate::services::structure_control_ops::*;
use eve_db::{
    CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents, UpwellStructureRecord,
};
use eve_machonet::{service_roles, SessionState};
use eve_sde::{InvType, SdeRegistry};
use std::collections::HashSet;
use std::sync::{Arc, OnceLock};

#[cfg(test)]
#[path = "structure_control_boarding_and_defense_unit_tests.rs"]
mod structure_control_boarding_and_defense_unit_tests;

pub(super) static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

pub(super) fn test_ctx() -> ServerContext {
    if let Some(sde) = SHARED_SDE.get() {
        ServerContext::with_sde_and_repository(
            Arc::clone(sde),
            Arc::new(eve_db::WorldRepository::with_demo_seed()),
        )
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
    } else {
        let ctx = ServerContext::new();
        let _ = SHARED_SDE.set(Arc::clone(&ctx.sde));
        ctx
    }
}

pub(super) fn clone_type_with(
    sde: &mut Arc<SdeRegistry>,
    base_tid: u32,
    new_tid: u32,
    group_id: u32,
    category_id: u32,
    attrs: &[(u16, f64)],
) {
    let reg = Arc::make_mut(sde);
    let mut rec: InvType = reg.types.values().next().cloned().expect("base sde type");
    if let Some(b) = reg.types.get(&base_tid) {
        rec = b.clone();
    }
    rec.type_id = new_tid;
    rec.group_id = group_id;
    rec.category_id = category_id;
    rec.name = format!("ExtraCtrlType_{new_tid}");
    rec.radius = 5_000.0;
    rec.mass = 1.0e11;
    rec.default_attributes.clear();
    for &(k, v) in attrs {
        rec.default_attributes.insert(k, v);
    }
    reg.types.insert(new_tid, rec);
}

pub(super) fn make_session(
    char_id: i64,
    corp_id: i64,
    sys_id: u32,
    ship_id: Option<i64>,
    structure_id: Option<i64>,
    role: u64,
) -> SessionState {
    let mut s = SessionState::new(720);
    s.user_id = Some(char_id as u64);
    s.character_id = Some(char_id);
    s.corporation_id = Some(corp_id);
    s.solar_system_id = Some(sys_id);
    s.ship_id = ship_id;
    s.structure_id = structure_id;
    s.corp_role = role;
    s
}

pub(super) fn seed_pilot_and_structure(
    ctx: &ServerContext,
    char_id: i64,
    corp_id: i64,
    ally_id: Option<i64>,
    sys_id: u32,
    ship_id: i64,
    sid: i64,
    st_tid: u32,
    is_ceo: bool,
) {
    ctx.db.with_mut_snapshot(|snap| {
        let corp = snap
            .corporations
            .entry(corp_id)
            .or_insert_with(|| { let mut __corp = CorporationRecord::default();
                __corp.corporation_id = corp_id;
                __corp.name = format!("Corp {corp_id}");
                __corp.ticker = "CTL".into();
                __corp.ceo_id = if is_ceo { char_id } else { char_id + 500 };
                __corp.alliance_id = ally_id;
                __corp.hq_station_id = Some(60003760);
                __corp.tax_rate = 0.0;
                __corp.members = HashSet::from([char_id]);
                __corp.set_division_balance(1, IskCents(0));
                __corp });
        corp.members.insert(char_id);
        if is_ceo {
            corp.member_roles.insert(char_id, u64::MAX);
        }
        let mut ch = snap.characters.values().next().cloned().expect("demo char");
        ch.character_id = char_id;
        ch.name = format!("Pilot {char_id}");
        ch.corporation_id = corp_id;
        ch.alliance_id = ally_id;
        ch.solar_system_id = sys_id;
        ch.station_id = None;
        ch.structure_id = Some(sid);
        ch.active_ship_id = ship_id;
        ch.home_station_id = 60003760;
        ch.balance = IskCents(100_000);
        snap.characters.insert(char_id, ch);
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 600,
                owner_id: char_id,
                location_id: sid,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.structures
            .entry(sid)
            .or_insert_with(|| UpwellStructureRecord {
                structure_id: sid,
                type_id: st_tid,
                owner_corp_id: corp_id,
                solar_system_id: sys_id,
                name: "Control Citadel".into(),
                position: [1_000_000_000.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                shield_hp: 500_000.0,
                armor_hp: 500_000.0,
                hull_hp: 500_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            });
    });
}

#[test]
fn can_character_control_structure_with_profile_acl_and_block_matrix() {
    let ctx = test_ctx();
    let sid = 10_001_100_001i64;
    let corp_id = 98_601i64;
    let ally_id = 99_001_601i64;
    let char_id = 6001i64;
    seed_pilot_and_structure(
        &ctx,
        char_id,
        corp_id,
        Some(ally_id),
        30000142,
        70_001,
        sid,
        35832,
        false,
    );

    // Non-owning corp without defense ACL returns false
    ctx.db.with_snapshot(|snap| {
        let ext = load_ext_state(snap);
        let st = &snap.structures[&sid];
        assert!(!can_character_control_structure_with_profile(
            snap,
            &ext,
            st,
            None,
            char_id + 999
        ));
    });

    // Profile explicit character / corp / alliance blocks
    for mode in 0..4 {
        ctx.db.with_mut_snapshot(|snap| {
            let mut ext = load_ext_state(snap);
            let mut prof = StructureAccessProfile {
                profile_id: 700,
                owner_corp_id: corp_id,
                defense_allowed_characters: HashSet::from([char_id]),
                ..Default::default()
            };
            match mode {
                0 => {
                    prof.defense_blocked_characters.insert(char_id);
                }
                1 => {
                    prof.blocked_characters.insert(char_id);
                }
                2 => {
                    prof.blocked_corporations.insert(corp_id);
                }
                _ => {
                    prof.blocked_alliances.insert(ally_id);
                }
            }
            ext.access_profiles.insert(sid, prof);
            save_ext_state(snap, &ext);
            let st = &snap.structures[&sid];
            assert!(!can_character_control_structure_with_profile(
                snap,
                &ext,
                st,
                ext.access_profiles.get(&sid),
                char_id,
            ));
        });
    }

    // Defense group -1 block for character, corp, or alliance overrides defense_allowed_characters
    for blocked_entity in [char_id, corp_id, ally_id] {
        ctx.db.with_mut_snapshot(|snap| {
            let mut ext = load_ext_state(snap);
            let mut grp = AccessGroupRecord {
                group_id: 55,
                owner_corp_id: corp_id,
                ..Default::default()
            };
            grp.members.insert(blocked_entity, -1);
            ext.owner_groups.insert(55, grp);
            let prof = StructureAccessProfile {
                profile_id: 701,
                owner_corp_id: corp_id,
                defense_group_ids: HashSet::from([55]),
                defense_allowed_characters: HashSet::from([char_id]),
                ..Default::default()
            };
            ext.access_profiles.insert(sid, prof);
            save_ext_state(snap, &ext);
            let st = &snap.structures[&sid];
            assert!(!can_character_control_structure_with_profile(
                snap,
                &ext,
                st,
                ext.access_profiles.get(&sid),
                char_id,
            ));
        });
    }

    // Positive defense group grants via public(0), char, corp, and alliance even for non-owning corp
    for granted_entity in [0i64, char_id, corp_id, ally_id] {
        ctx.db.with_mut_snapshot(|snap| {
            let mut ext = load_ext_state(snap);
            let mut grp = AccessGroupRecord {
                group_id: 56,
                owner_corp_id: corp_id,
                ..Default::default()
            };
            grp.members.insert(granted_entity, 0);
            ext.owner_groups.insert(56, grp);
            let prof = StructureAccessProfile {
                profile_id: 702,
                owner_corp_id: corp_id,
                defense_group_ids: HashSet::from([56]),
                ..Default::default()
            };
            ext.access_profiles.insert(sid, prof);
            save_ext_state(snap, &ext);
            let mut other_st = snap.structures[&sid].clone();
            other_st.owner_corp_id = corp_id + 999;
            assert!(can_character_control_structure_with_profile(
                snap,
                &ext,
                &other_st,
                ext.access_profiles.get(&sid),
                char_id,
            ));
        });
    }

    // Non-empty defense_group_ids without match denies pilot without corp roles
    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        let grp = AccessGroupRecord {
            group_id: 57,
            owner_corp_id: corp_id,
            ..Default::default()
        };
        ext.owner_groups.insert(57, grp);
        let prof = StructureAccessProfile {
            profile_id: 703,
            owner_corp_id: corp_id,
            defense_group_ids: HashSet::from([57]),
            ..Default::default()
        };
        ext.access_profiles.insert(sid, prof);
        save_ext_state(snap, &ext);
        let st = &snap.structures[&sid];
        assert!(!can_character_control_structure_with_profile(
            snap,
            &ext,
            st,
            ext.access_profiles.get(&sid),
            char_id,
        ));
    });
}
