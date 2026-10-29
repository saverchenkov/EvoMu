//! Behavioural parity tests for `feat:fw.*` / `feat:sovereignty.*` rows.
//!
//! Only rows whose every acceptance clause is exercised here are tagged; see the final report for
//! the rows that remain open.

use crate::common;

use eve_db::{CorporationRecord, IskCents, UpwellStructureRecord};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::sov_services::{self, TYPE_ORBITAL_SKYHOOK, TYPE_SOVEREIGNTY_HUB};
use eve_server::services::station_structure::structure_lifecycle as life;
use std::collections::{HashMap, HashSet};

const PILOT: i64 = 90_000_001;
const OTHER: i64 = 90_000_002;
const CALDARI: i64 = 500_001;
const MINMATAR: i64 = 500_002;
const JOVE_NO_MILITIA: i64 = 500_005;
const DAY: u64 = 86_400;

async fn select(ctx: &ServerContext, sess: &mut SessionState, user: &str, pass: &str, cid: i64) {
    common::login_pilot(ctx, sess, user, pass).await;
    ctx.dispatch_rpc(sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(cid)], &HashMap::new(), None)
        .await
        .expect("SelectCharacterID");
}

async fn call(
    ctx: &ServerContext,
    sess: &mut SessionState,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, eve_proto::BridgeRpcError> {
    ctx.dispatch_rpc(sess, "fwCharacterEnlistmentMgr", None, method, args, &HashMap::new(), None).await
}

async fn sov_call(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, eve_proto::BridgeRpcError> {
    ctx.dispatch_rpc(sess, svc, None, method, args, &HashMap::new(), None).await
}

fn enlistment(rep: &PyRep) -> Option<i64> {
    match rep {
        PyRep::Tuple(v) if v.len() == 3 => v[0].as_i64(),
        other => panic!("GetMyEnlistment must return a 3-tuple, got {other:?}"),
    }
}

fn unix_to_filetime(unix: u64) -> i64 {
    (unix as i64 + 11_644_473_600) * 10_000_000
}

fn make_system_dynamic_state(sys_id: u32) -> eve_db::SystemDynamicState {
    eve_db::SystemDynamicState {
        solar_system_id: sys_id,
        dbs_multiplier: 1.0,
        ess_main_bank: eve_db::IskCents(0),
        ess_reserve_bank: eve_db::IskCents(0),
        ess_linked_character_id: None,
        ess_link_started_tick: None,
        sov_alliance_id: None,
        military_index: 0,
        industrial_index: 0,
        strategic_index: 0,
        fw_contested_pct: 0.0,
        insurgency_corruption_stage: 1,
        insurgency_suppression_stage: 0,
        wormhole_remaining_mass_kg: 0.0,
        wormhole_destination_system_id: None,
        signature_position: None,
        fw_complex_position: None,
        fw_capture_seconds_elapsed: 0,
        fw_capture_duration_seconds: 0,
        last_fw_capture_tick: 0,
        last_sov_claim_tick: 0,
    }
}

#[tokio::test]
async fn fw_direct_enlistment_standings_and_leave_cooldown() {
    // parity: feat:fw.enlist, feat:fw.leave-militia
    eve_testkit::attest(&["feat:fw.enlist", "feat:fw.leave-militia"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut sess = SessionState::new(41_001);
    select(&ctx, &mut sess, "demo", "demo-secret", PILOT).await;

    // Not enlisted initially; no cooldown.
    let mine = call(&ctx, &mut sess, "GetMyEnlistment", &[]).await.expect("GetMyEnlistment");
    assert_eq!(enlistment(&mine), None);
    let cd0 = call(&ctx, &mut sess, "GetMyDirectEnlistmentCooldownTimestamp", &[]).await.unwrap();
    assert_eq!(cd0, PyRep::None);

    // A faction without a militia (SDE: no militia corporation) is refused.
    let bad = call(&ctx, &mut sess, "CreateMyDirectEnlistment", &[PyRep::Integer(JOVE_NO_MILITIA)])
        .await
        .expect_err("non-militia faction must be refused");
    assert_eq!(bad.code, "InvalidWarFaction");

    // Negative standings towards the faction block enlistment.
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&PILOT).unwrap().standings.insert(MINMATAR, -0.5);
    });
    let low = call(&ctx, &mut sess, "CreateMyDirectEnlistment", &[PyRep::Integer(MINMATAR)])
        .await
        .expect_err("negative standing must block enlistment");
    assert_eq!(low.code, "FwStandingTooLow");
    assert_eq!(ctx.db.get_character(PILOT).unwrap().war_faction_id, None);

    // Exactly zero standing (non-negative) is allowed.
    ctx.drain_notifications_for_char(PILOT);
    call(&ctx, &mut sess, "CreateMyDirectEnlistment", &[PyRep::Integer(CALDARI)])
        .await
        .expect("enlist with non-negative standing");
    assert_eq!(ctx.db.get_character(PILOT).unwrap().war_faction_id, Some(CALDARI));
    assert_eq!(sess.war_faction_id, Some(CALDARI));
    let mine = call(&ctx, &mut sess, "GetMyEnlistment", &[]).await.unwrap();
    assert_eq!(enlistment(&mine), Some(CALDARI));
    let notifs = ctx.drain_notifications_for_char(PILOT);
    let upd = notifs.iter().find(|n| n.event == "OnEnlistmentStateUpdated").expect("OnEnlistmentStateUpdated");
    assert_eq!(upd.args.first().and_then(|a| a.as_i64()), Some(CALDARI));

    // Enlisting twice is refused.
    let dup = call(&ctx, &mut sess, "CreateMyDirectEnlistment", &[PyRep::Integer(CALDARI)])
        .await
        .expect_err("already enlisted");
    assert_eq!(dup.code, "FwAlreadyEnlisted");

    // Leaving clears enlistment and starts the cooldown reported by the timestamp RPC.
    let left_at = ctx.now_unix();
    call(&ctx, &mut sess, "RemoveMyDirectEnlistment", &[]).await.expect("leave militia");
    assert_eq!(ctx.db.get_character(PILOT).unwrap().war_faction_id, None);
    assert_eq!(sess.war_faction_id, None);
    let cd = call(&ctx, &mut sess, "GetMyDirectEnlistmentCooldownTimestamp", &[]).await.unwrap();
    assert_eq!(cd.as_i64(), Some(unix_to_filetime(left_at + DAY)));

    // Leaving again (not enlisted) is refused.
    let not_enlisted = call(&ctx, &mut sess, "RemoveMyDirectEnlistment", &[]).await.expect_err("not enlisted");
    assert_eq!(not_enlisted.code, "FwNotEnlisted");

    // During the cooldown re-enlistment is refused.
    ctx.advance_virtual_clock(DAY - 60);
    let during = call(&ctx, &mut sess, "CreateMyDirectEnlistment", &[PyRep::Integer(CALDARI)])
        .await
        .expect_err("cooldown active");
    assert_eq!(during.code, "FwEnlistmentCooldown");

    // After the cooldown expires the timestamp clears and enlistment works again.
    ctx.advance_virtual_clock(61);
    let cd_after = call(&ctx, &mut sess, "GetMyDirectEnlistmentCooldownTimestamp", &[]).await.unwrap();
    assert_eq!(cd_after, PyRep::None);
    call(&ctx, &mut sess, "CreateMyDirectEnlistment", &[PyRep::Integer(CALDARI)])
        .await
        .expect("re-enlist after cooldown");
    assert_eq!(ctx.db.get_character(PILOT).unwrap().war_faction_id, Some(CALDARI));
}

#[tokio::test]
async fn fw_corp_allowed_enlistment_factions_gate_member_enlistment() {
    // parity: feat:fw.corp-enlist
    eve_testkit::attest(&["feat:fw.corp-enlist"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let corp_id: i64 = 98_000_777;
    ctx.db.with_mut_snapshot(|s| {
        s.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                name: "Enlistment Test Corp".into(),
                ticker: "ETC".into(),
                ceo_id: OTHER,
                members: HashSet::from([PILOT, OTHER]),
                master_wallet: IskCents::from_isk(0),
                ..Default::default()
            },
        );
        for cid in [PILOT, OTHER] {
            s.characters.get_mut(&cid).unwrap().corporation_id = corp_id;
        }
    });
    let mut member = SessionState::new(41_010);
    select(&ctx, &mut member, "demo", "demo-secret", PILOT).await;
    let mut ceo = SessionState::new(41_011);
    select(&ctx, &mut ceo, "other", "other-secret", OTHER).await;

    // Nothing configured yet.
    let none = call(&ctx, &mut member, "GetCorpAllowedEnlistmentFactions", &[PyRep::Integer(corp_id)]).await.unwrap();
    assert_eq!(none, PyRep::None);

    // A plain member (no director role, not CEO) may not change the setting.
    let denied = call(&ctx, &mut member, "SetMyCorpAllowedEnlistmentFactions", &[PyRep::List(vec![PyRep::Integer(CALDARI)])])
        .await
        .expect_err("member must not set allowed factions");
    assert_eq!(denied.code, "CorpRoleDenied");

    // Non-militia factions are rejected even for the CEO.
    let bad = call(&ctx, &mut ceo, "SetMyCorpAllowedEnlistmentFactions", &[PyRep::List(vec![PyRep::Integer(JOVE_NO_MILITIA)])])
        .await
        .expect_err("non-militia faction rejected");
    assert_eq!(bad.code, "InvalidWarFaction");

    // CEO restricts members to the Caldari militia; the setting is persisted and readable.
    let set = call(&ctx, &mut ceo, "SetMyCorpAllowedEnlistmentFactions", &[PyRep::List(vec![PyRep::Integer(CALDARI)])])
        .await
        .expect("CEO sets allowed factions");
    assert_eq!(set, PyRep::List(vec![PyRep::Integer(CALDARI)]));
    let got = call(&ctx, &mut member, "GetCorpAllowedEnlistmentFactions", &[PyRep::Integer(corp_id)]).await.unwrap();
    assert_eq!(got, PyRep::List(vec![PyRep::Integer(CALDARI)]));
    assert!(ctx
        .drain_notifications_for_char(PILOT)
        .iter()
        .any(|n| n.event == "OnCorpAllowedEnlistmentFactionsSet"));

    // Member cannot join a faction the corp does not allow, but can join the allowed one.
    let blocked = call(&ctx, &mut member, "CreateMyDirectEnlistment", &[PyRep::Integer(MINMATAR)])
        .await
        .expect_err("faction not allowed by corp");
    assert_eq!(blocked.code, "FwFactionNotAllowedByCorp");
    assert_eq!(ctx.db.get_character(PILOT).unwrap().war_faction_id, None);
    call(&ctx, &mut member, "CreateMyDirectEnlistment", &[PyRep::Integer(CALDARI)])
        .await
        .expect("allowed faction");
    assert_eq!(ctx.db.get_character(PILOT).unwrap().war_faction_id, Some(CALDARI));

    // A director (not CEO) may also update the list; an empty list blocks all militias.
    ctx.db.with_mut_snapshot(|s| {
        s.corporations
            .get_mut(&corp_id)
            .unwrap()
            .member_roles
            .insert(PILOT, eve_db::corp_roles::ROLE_DIRECTOR);
    });
    call(&ctx, &mut member, "RemoveMyDirectEnlistment", &[]).await.expect("leave");
    ctx.advance_virtual_clock(DAY + 1);
    let cleared = call(&ctx, &mut member, "SetMyCorpAllowedEnlistmentFactions", &[PyRep::List(vec![])])
        .await
        .expect("director sets empty list");
    assert_eq!(cleared, PyRep::List(vec![]));
    let blocked_all = call(&ctx, &mut member, "CreateMyDirectEnlistment", &[PyRep::Integer(CALDARI)])
        .await
        .expect_err("empty allow-list blocks all militias");
    assert_eq!(blocked_all.code, "FwFactionNotAllowedByCorp");

    // NPC-corp members cannot configure corp enlistment.
    let mut npc_sess = SessionState::new(41_012);
    ctx.db.with_mut_snapshot(|s| {
        // 1000045 is an SDE NPC corporation (not a player corp in `corporations`).
        s.characters.get_mut(&OTHER).unwrap().corporation_id = 1_000_045;
    });
    select(&ctx, &mut npc_sess, "other", "other-secret", OTHER).await;
    let npc_denied = call(&ctx, &mut npc_sess, "SetMyCorpAllowedEnlistmentFactions", &[PyRep::List(vec![PyRep::Integer(CALDARI)])])
        .await
        .expect_err("NPC corp cannot configure");
    assert_eq!(npc_denied.code, "CorpRoleDenied");
}

#[tokio::test]
async fn sov_hub_anchoring_claims_unclaimed_nullsec_and_emits_notifications() {
    // parity: feat:sovereignty.sov-hub-anchor, feat:sovereignty.sov-changed-notifications
    eve_testkit::attest(&[
        "feat:sovereignty.sov-hub-anchor",
        "feat:sovereignty.sov-changed-notifications",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let sys_id: u32 = 30000208; // LZ-6SU (null-sec system)
    let corp_id: i64 = 98_000_100;
    let alliance_id: i64 = 99_000_200;
    let hub_structure_id: i64 = 600_000_001;

    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|s| {
        s.alliances.insert(
            alliance_id,
            eve_db::AllianceRecord {
                alliance_id,
                name: "Test Sov Alliance".into(),
                short_name: "TSA".into(),
                executor_corp_id: corp_id,
                member_corp_ids: HashSet::from([corp_id]),
            },
        );
        s.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                alliance_id: Some(alliance_id),
                name: "Test Sov Corp".into(),
                ticker: "TSC".into(),
                ceo_id: PILOT,
                members: HashSet::from([PILOT]),
                master_wallet: IskCents::from_isk(0),
                ..Default::default()
            },
        );
        s.characters.get_mut(&PILOT).unwrap().corporation_id = corp_id;
        s.characters.get_mut(&PILOT).unwrap().alliance_id = Some(alliance_id);
        s.characters.get_mut(&PILOT).unwrap().solar_system_id = sys_id;

        let st = s.system_states.entry(sys_id).or_insert_with(|| make_system_dynamic_state(sys_id));
        st.sov_alliance_id = None;

        s.structures.insert(
            hub_structure_id,
            UpwellStructureRecord {
                structure_id: hub_structure_id,
                type_id: TYPE_SOVEREIGNTY_HUB,
                owner_corp_id: corp_id,
                solar_system_id: sys_id,
                name: "LZ-6SU Sovereignty Hub".into(),
                position: [0.0, 0.0, 0.0],
                state: life::STATE_ONLINING.into(),
                quantum_core_installed: true,
                fuel_blocks: 1000,
                shield_hp: 1_000_000.0,
                armor_hp: 1_000_000.0,
                hull_hp: 1_000_000.0,
                reinforced_until_unix: now + 50,
                last_damage_tick: 0,
                last_damage_unix: 0, self_destruct_at_unix: None,
            },
        );
    });

    let mut sess = SessionState::new(42_001);
    select(&ctx, &mut sess, "demo", "demo-secret", PILOT).await;
    sess.solar_system_id = Some(sys_id);
    sess.corporation_id = Some(corp_id);
    sess.alliance_id = Some(alliance_id);

    assert_eq!(ctx.db.with_snapshot(|s| s.system_states.get(&sys_id).and_then(|st| st.sov_alliance_id)), None);

    ctx.drain_notifications_for_char(PILOT);
    ctx.advance_virtual_clock(100);
    life::sync_all_structures(&ctx);

    let st = ctx.db.with_snapshot(|s| s.structures.get(&hub_structure_id).cloned()).expect("hub exists");
    assert_eq!(st.state, life::STATE_SHIELD_VULNERABLE);

    let sys_st = ctx.db.with_snapshot(|s| s.system_states.get(&sys_id).cloned()).expect("system state exists");
    assert_eq!(sys_st.sov_alliance_id, Some(alliance_id));
    assert_eq!(sys_st.military_index, 0);
    assert_eq!(sys_st.industrial_index, 0);
    assert_eq!(sys_st.strategic_index, 0);

    let notifs = ctx.drain_notifications_for_char(PILOT);
    let sov_notif = notifs.iter().find(|n| n.event == "OnSovereigntyChanged").expect("OnSovereigntyChanged");
    assert_eq!(sov_notif.args.first().and_then(|a| a.as_i64()), Some(sys_id as i64));
    assert_eq!(sov_notif.args.get(1).and_then(|a| a.as_i64()), Some(alliance_id));

    sov_services::claim_system_sovereignty(&ctx, sys_id, alliance_id, corp_id, hub_structure_id)
        .expect("claim_system_sovereignty");
    let notifs2 = ctx.drain_notifications_for_char(PILOT);
    assert!(notifs2.iter().any(|n| n.event == "OnSolarSystemSovStructuresUpdated"));
}

#[tokio::test]
async fn sov_info_adm_scaling_dev_indices_and_map_data() {
    // parity: feat:sovereignty.sov-info, feat:sovereignty.sov-adm, feat:sovereignty.sov-map-data
    eve_testkit::attest(&[
        "feat:sovereignty.sov-info",
        "feat:sovereignty.sov-adm",
        "feat:sovereignty.sov-map-data",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let sys_id: u32 = 30000208;
    let corp_id: i64 = 98_000_100;
    let alliance_id: i64 = 99_000_200;
    let hub_id: i64 = 600_000_002;
    let skyhook_id: i64 = 600_000_003;

    ctx.db.with_mut_snapshot(|s| {
        s.alliances.insert(
            alliance_id,
            eve_db::AllianceRecord {
                alliance_id,
                name: "Test Sov Alliance".into(),
                short_name: "TSA".into(),
                executor_corp_id: corp_id,
                member_corp_ids: HashSet::from([corp_id]),
            },
        );
        s.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                alliance_id: Some(alliance_id),
                name: "Test Sov Corp".into(),
                ticker: "TSC".into(),
                ceo_id: PILOT,
                members: HashSet::from([PILOT]),
                master_wallet: IskCents::from_isk(0),
                ..Default::default()
            },
        );
        s.characters.get_mut(&PILOT).unwrap().corporation_id = corp_id;
        s.characters.get_mut(&PILOT).unwrap().alliance_id = Some(alliance_id);
        s.characters.get_mut(&PILOT).unwrap().solar_system_id = sys_id;

        let st = s.system_states.entry(sys_id).or_insert_with(|| make_system_dynamic_state(sys_id));
        st.sov_alliance_id = Some(alliance_id);
        st.military_index = 2;
        st.industrial_index = 2;
        st.strategic_index = 1;

        s.structures.insert(
            hub_id,
            UpwellStructureRecord {
                structure_id: hub_id,
                type_id: TYPE_SOVEREIGNTY_HUB,
                owner_corp_id: corp_id,
                solar_system_id: sys_id,
                name: "Hub".into(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 500,
                shield_hp: 1_000_000.0,
                armor_hp: 1_000_000.0,
                hull_hp: 1_000_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0, self_destruct_at_unix: None,
            },
        );
        s.structures.insert(
            skyhook_id,
            UpwellStructureRecord {
                structure_id: skyhook_id,
                type_id: TYPE_ORBITAL_SKYHOOK,
                owner_corp_id: corp_id,
                solar_system_id: sys_id,
                name: "Skyhook".into(),
                position: [100.0, 100.0, 100.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 0,
                shield_hp: 500_000.0,
                armor_hp: 500_000.0,
                hull_hp: 500_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0, self_destruct_at_unix: None,
            },
        );
        s.structure_vulnerability_windows.insert(hub_id, (1000, 5000));
        s.structure_vulnerability_windows.insert(skyhook_id, (1000, 5000));
    });

    let mut sess = SessionState::new(42_002);
    select(&ctx, &mut sess, "demo", "demo-secret", PILOT).await;
    sess.solar_system_id = Some(sys_id);
    sess.corporation_id = Some(corp_id);
    sess.alliance_id = Some(alliance_id);

    let dev_indices = sov_call(&ctx, &mut sess, "devIndexManager", "GetAllDevelopmentIndices", &[])
        .await
        .expect("GetAllDevelopmentIndices");
    match dev_indices {
        PyRep::Dict(entries) => {
            let item = entries.iter().find(|(k, _)| k.as_i64() == Some(sys_id as i64)).expect("sys entry");
            assert_eq!(item.1.dict_get("military").and_then(|v| v.as_i64()), Some(2));
            assert_eq!(item.1.dict_get("industrial").and_then(|v| v.as_i64()), Some(2));
            assert_eq!(item.1.dict_get("strategic").and_then(|v| v.as_i64()), Some(1));
            let adm = item.1.dict_get("adm").and_then(|v| v.as_f64()).expect("adm float");
            assert!((adm - 3.8).abs() < 0.05);

            let base_entosis_time = 600;
            let scaled_time = sov_services::scale_entosis_capture_seconds(base_entosis_time, adm);
            assert_eq!(scaled_time, (600.0_f64 * 3.8_f64).round() as u64);
        }
        _other => panic!("expected Dict, got {_other:?}"),
    }

    ctx.drain_notifications_for_char(PILOT);
    ctx.db.with_mut_snapshot(|s| {
        let st = s.system_states.get_mut(&sys_id).unwrap();
        st.military_index = 3;
        st.industrial_index = 3;
        st.strategic_index = 2;
    });
    sov_services::emit_dev_index_changed(&ctx, sys_id);

    let notifs = ctx.drain_notifications_for_char(PILOT);
    let dev_notif = notifs.iter().find(|n| n.event == "OnSolarSystemDevIndexChanged").expect("OnSolarSystemDevIndexChanged");
    assert_eq!(dev_notif.args, vec![PyRep::Integer(sys_id as i64)]);

    let info = sov_call(&ctx, &mut sess, "sovMgr", "GetSystemSovereigntyInfo", &[PyRep::Integer(sys_id as i64)])
        .await
        .expect("GetSystemSovereigntyInfo");
    assert_eq!(info.dict_get("allianceID").and_then(|v| v.as_i64()), Some(alliance_id));
    assert_eq!(info.dict_get("infrastructureHubID").and_then(|v| v.as_i64()), Some(hub_id));

    let structs_info = sov_call(&ctx, &mut sess, "sovMgr", "GetSovStructuresInfoForSolarSystem", &[PyRep::Integer(sys_id as i64)])
        .await
        .expect("GetSovStructuresInfoForSolarSystem");
    assert_eq!(structs_info.dict_get("allianceID").and_then(|v| v.as_i64()), Some(alliance_id));
    if let Some(PyRep::List(list)) = structs_info.dict_get("structures") {
        assert_eq!(list.len(), 2);
        let hub_rep = list.iter().find(|s| s.dict_get("structureID").and_then(|v| v.as_i64()) == Some(hub_id)).unwrap();
        assert_eq!(hub_rep.dict_get("corporationID").and_then(|v| v.as_i64()), Some(corp_id));
        assert_eq!(hub_rep.dict_get("vulnerableStartTime").and_then(|v| v.as_i64()), Some(1000));
        assert_eq!(hub_rep.dict_get("vulnerableEndTime").and_then(|v| v.as_i64()), Some(5000));
    } else {
        panic!("expected structures list");
    }

    let map_data = sov_call(&ctx, &mut sess, "map", "GetCurrentSovData", &[])
        .await
        .expect("GetCurrentSovData");
    if let PyRep::List(list) = map_data {
        let entry = list.iter().find(|e| e.dict_get("solarSystemID").and_then(|v| v.as_i64()) == Some(sys_id as i64)).expect("map entry for system");
        assert_eq!(entry.dict_get("allianceID").and_then(|v| v.as_i64()), Some(alliance_id));
        assert_eq!(entry.dict_get("corporationID").and_then(|v| v.as_i64()), Some(corp_id));
        assert_eq!(entry.dict_get("infrastructureHubID").and_then(|v| v.as_i64()), Some(hub_id));
    } else {
        panic!("expected list from map.GetCurrentSovData");
    }
}
