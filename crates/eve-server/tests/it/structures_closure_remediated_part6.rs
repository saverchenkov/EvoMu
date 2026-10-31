//! Remediation integration tests for `BF-01` (Upwell `Anchor` cross-corp `profileID` rejection,
//! default corp profile auto-attachment, and `DeleteProfile` owner-corp scoping) and `BF-02`
//! (per-service `(groupID, value)` ACL mappings preserving Docking ACLs and enforcing per-group
//! service access & tax rates across Market, Reprocessing, Clone Bay, Industry, and Jump Bridge).

use eve_db::{
    AllianceRecord, CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents,
    TrainedSkillRecord, UpwellStructureRecord,
};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::services::station_structure::structure_lifecycle as life;
use eve_server::services::structure_profile_services as svc_acl;
use glam::DVec3;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

const OWNER: i64 = 90_000_001;
const CORP_B_CEO: i64 = 90_000_061;
const PILOT_DOCK_ONLY: i64 = 90_000_062;
const PILOT_VIP: i64 = 90_000_063;
const PILOT_STD: i64 = 90_000_064;
const PILOT_BOTH: i64 = 90_000_065;
const RANCER: u32 = 30_002_718;
const SOV_NULL_A: u32 = 30_004_759; // 1DQ1-A
const SOV_NULL_B: u32 = 30_004_760; // T5ZI-S
const RIFTER_BP: i64 = 691;

async fn rpc(ctx: &ServerContext, sess: &mut SessionState, service: &str, method: &str, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None).await
}

fn kv_i64(rep: &PyRep, key: &str) -> i64 {
    rep.dict_get(key).and_then(|v| v.as_i64()).unwrap_or(0)
}

fn new_ctx() -> Arc<ServerContext> {
    let ctx = Arc::new(ServerContext::new());
    ctx.set_clock(Arc::new(eve_db::VirtualServerClock::with_fixed_base(1_800_000_000)));
    ctx
}

fn ensure_account(snap: &mut eve_db::WorldStateSnapshot, uid: u64) {
    if !snap.accounts.values().any(|a| a.user_id == uid) {
        if let Some(mut acc) = snap.accounts.get("demo").cloned() {
            acc.user_id = uid;
            acc.username = format!("pilot_{uid}");
            snap.accounts.insert(acc.username.clone(), acc);
        }
    }
}

async fn setup_corp_ceo(
    ctx: &ServerContext,
    sess_id: u64,
    char_id: i64,
    corp_id: i64,
    name: &str,
    ticker: &str,
    sys_id: u32,
    ship_id: i64,
    pos: DVec3,
) -> SessionState {
    let uid = char_id as u64;
    ctx.db.with_mut_snapshot(|snap| {
        ensure_account(snap, uid);
        let mut ch = snap.characters.get(&OWNER).cloned().unwrap();
        ch.character_id = char_id;
        ch.user_id = uid;
        ch.corporation_id = corp_id;
        ch.solar_system_id = sys_id;
        ch.station_id = None;
        ch.structure_id = None;
        ch.active_ship_id = ship_id;
        ch.balance = IskCents::from_isk(500_000_000);
        snap.characters.insert(char_id, ch);

        let mut c = CorporationRecord::default();
        c.corporation_id = corp_id;
        c.name = name.into();
        c.ticker = ticker.into();
        c.ceo_id = char_id;
        c.master_wallet = IskCents::from_isk(100_000_000);
        c.members.insert(char_id);
        c.member_roles.insert(char_id, u64::MAX);
        snap.corporations.insert(corp_id, c);

        snap.items.insert(ship_id, InventoryItemRecord {
            item_id: ship_id, type_id: 648, owner_id: char_id, location_id: i64::from(sys_id),
            flag: InventoryFlag::None, quantity: 1, singleton: true, version: 1, ..Default::default()
        });
    });
    ctx.systems.get_or_spawn(sys_id).spawn_ball(Ball {
        id: ship_id, type_id: 648, position: pos, velocity: DVec3::ZERO,
        max_velocity: 200.0, inertia: 1.0, mass: 1_500_000.0, radius: 120.0,
        speed_fraction: 0.0, mode: MovementMode::Stop,
    }).await.expect("spawn_ball");
    let mut sess = SessionState::new(sess_id);
    sess.user_id = Some(uid);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    sess.corp_role = u64::MAX;
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);
    sess
}

fn seed_docked_pilot(
    ctx: &ServerContext,
    sess_id: u64,
    char_id: i64,
    corp_id: i64,
    alliance_id: Option<i64>,
    sys_id: u32,
    st_id: i64,
    ship_id: i64,
    ship_type_id: u32,
) -> SessionState {
    let uid = char_id as u64;
    let reg_id = ctx.sde.solar_systems.get(&sys_id).map(|s| s.region_id).unwrap_or(10_000_002);
    ctx.db.with_mut_snapshot(|snap| {
        ensure_account(snap, uid);
        let mut ch = snap.characters.get(&OWNER).cloned().unwrap();
        ch.character_id = char_id;
        ch.user_id = uid;
        ch.corporation_id = corp_id;
        ch.alliance_id = alliance_id;
        ch.solar_system_id = sys_id;
        ch.region_id = reg_id;
        ch.station_id = None;
        ch.structure_id = Some(st_id);
        ch.active_ship_id = ship_id;
        ch.clone_grade = 2; // Omega
        ch.balance = IskCents::from_isk(100_000_000);
        snap.characters.insert(char_id, ch);

        if corp_id >= 98_000_000 && !snap.corporations.contains_key(&corp_id) {
            let mut c = CorporationRecord::default();
            c.corporation_id = corp_id;
            c.name = format!("Pilot Corp {corp_id}");
            c.ticker = "PCRP".into();
            c.ceo_id = char_id;
            c.alliance_id = alliance_id;
            c.members.insert(char_id);
            snap.corporations.insert(corp_id, c);
        } else if let Some(c) = snap.corporations.get_mut(&corp_id) {
            c.members.insert(char_id);
        }
        if let Some(aid) = alliance_id {
            snap.alliances
                .entry(aid)
                .or_insert_with(|| AllianceRecord {
                    alliance_id: aid,
                    name: format!("Alliance {aid}"),
                    short_name: "ALLY".into(),
                    executor_corp_id: corp_id,
                    member_corp_ids: HashSet::new(),
                })
                .member_corp_ids
                .insert(corp_id);
        }

        let sk = snap.skills.entry(char_id).or_default();
        sk.insert(24242, TrainedSkillRecord { type_id: 24242, level: 5, skill_points: 256_000.0 });
        sk.insert(3380, TrainedSkillRecord { type_id: 3380, level: 5, skill_points: 256_000.0 });
        sk.insert(3387, TrainedSkillRecord { type_id: 3387, level: 5, skill_points: 256_000.0 });

        snap.items.insert(ship_id, InventoryItemRecord {
            item_id: ship_id, type_id: ship_type_id, owner_id: char_id, location_id: st_id,
            flag: InventoryFlag::Hangar, quantity: 1, singleton: true, version: 1, ..Default::default()
        });
    });
    let mut s = SessionState::new(sess_id);
    s.user_id = Some(uid);
    s.character_id = Some(char_id);
    s.corporation_id = Some(corp_id);
    s.alliance_id = alliance_id;
    s.solar_system_id = Some(sys_id);
    s.structure_id = Some(st_id);
    s.ship_id = Some(ship_id);
    s
}

#[tokio::test]
async fn test_bf01_anchor_cross_corp_profile_rejected_default_attached_and_delete_scoped() {
    // parity: feat:structures.anchor-restrictions, feat:structures.profiles, rpc:structureDeployment.Anchor
    // Exercises cross-corp profileID rejection, default profile attachment, and DeleteProfile scoping.
    eve_testkit::attest(&[
        "feat:structures.anchor-restrictions",
        "feat:structures.profiles",
        "rpc:structureDeployment.Anchor",
    ]);

    let ctx = new_ctx();
    let corp_a = 98_901_001;
    let corp_b = 98_901_002;
    let pos_a = DVec3::new(5.0e10, 5.0e10, 5.0e10);
    let pos_b = DVec3::new(-5.0e10, 5.0e10, 5.0e10);
    let ship_a = 890_001_001;
    let ship_b = 890_001_002;
    let mut sess_a = setup_corp_ceo(&ctx, 9101, OWNER, corp_a, "Alpha Anchor Corp", "AANC", RANCER, ship_a, pos_a).await;
    let mut sess_b = setup_corp_ceo(&ctx, 9102, CORP_B_CEO, corp_b, "Bravo Victim Corp", "BVIC", RANCER, ship_b, pos_b).await;

    let prof_a_def = rpc(&ctx, &mut sess_a, "structureProfiles", "CreateProfile", &[PyRep::String("Alpha Default".into()), PyRep::String("Default".into())]).await.unwrap().as_i64().unwrap();
    rpc(&ctx, &mut sess_a, "structureProfiles", "SetDefaultProfile", &[PyRep::Integer(prof_a_def)]).await.unwrap();
    let prof_b = rpc(&ctx, &mut sess_b, "structureProfiles", "CreateProfile", &[PyRep::String("Bravo Private".into()), PyRep::String("Private".into())]).await.unwrap().as_i64().unwrap();

    let astrahus_cargo_id = 890_001_010;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(astrahus_cargo_id, InventoryItemRecord {
            item_id: astrahus_cargo_id, type_id: 35832, owner_id: OWNER, location_id: ship_a,
            flag: InventoryFlag::Cargo, quantity: 1, singleton: false, version: 1, ..Default::default()
        });
    });

    // 1. Cross-corp or missing profileID on Anchor MUST fail with StructureOwnershipViolation BEFORE consuming cargo or creating structure!
    let anchor_pos = PyRep::Tuple(vec![PyRep::Real(pos_a.x + 60_000.0), PyRep::Real(pos_a.y), PyRep::Real(pos_a.z)]);
    let rw = PyRep::Tuple(vec![PyRep::Integer(18), PyRep::Integer(2)]);
    for bad_pid in [prof_b, 70_999_999_i64] {
        let bad_err = rpc(
            &ctx,
            &mut sess_a,
            "structureDeployment",
            "Anchor",
            &[PyRep::Integer(astrahus_cargo_id), anchor_pos.clone(), PyRep::Integer(bad_pid), rw.clone(), PyRep::String("Hijacked Citadel".into())],
        )
        .await
        .expect_err("Cross-corp or missing profileID on Anchor must be rejected");
        assert_eq!(bad_err.code, "StructureOwnershipViolation");
        assert!(ctx.db.get_item(astrahus_cargo_id).is_some(), "Cargo item must remain intact on failed Anchor");
        assert!(ctx.db.with_snapshot(|s| s.structures.is_empty()), "No structure may be created on failed Anchor");
    }

    // 2. Omitting profileID automatically attaches corp_a's default profile (`prof_a_def`).
    let anchored = rpc(
        &ctx,
        &mut sess_a,
        "structureDeployment",
        "Anchor",
        &[PyRep::Integer(astrahus_cargo_id), anchor_pos, PyRep::None, rw, PyRep::String("Default Profile Citadel".into())],
    )
    .await
    .expect("Anchor with omitted profileID");
    let st_id = kv_i64(&anchored, "structureID");
    assert!(st_id > 1_000_000_000_000);
    assert!(ctx.db.get_item(astrahus_cargo_id).is_none(), "Cargo item consumed after valid Anchor");
    let attached_pid = ctx.db.with_snapshot(|snap| {
        eve_server::services::station_structure::structure_upwell_ops::load_ext_state(snap)
            .structure_profile_id
            .get(&st_id)
            .copied()
    });
    assert_eq!(attached_pid, Some(prof_a_def), "Default corp profile must be auto-attached when profileID is None");

    // 3. DeleteProfile refuses deleting prof_a_def while corp_a's structure uses it, while scoping ProfileInUse to owner_corp.
    assert_eq!(
        rpc(&ctx, &mut sess_a, "structureProfiles", "DeleteProfile", &[PyRep::Integer(prof_a_def)]).await.unwrap_err().code,
        "ProfileInUse"
    );

    // Even if a stale cross-corp mapping pointed at prof_b, Corp B is never DoS'd from deleting its own unused profile.
    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = eve_server::services::station_structure::structure_upwell_ops::load_ext_state(snap);
        ext.structure_profile_id.insert(1_000_999_999_001, prof_b);
        eve_server::services::station_structure::structure_upwell_ops::save_ext_state(snap, &ext);
    });
    assert_eq!(
        rpc(&ctx, &mut sess_b, "structureProfiles", "DeleteProfile", &[PyRep::Integer(prof_b)]).await.unwrap(),
        PyRep::Bool(true)
    );
}

#[tokio::test]
async fn test_bf02_per_service_acl_bindings_preserve_docking_and_enforce_group_taxes() {
    // parity: feat:structures.profiles, feat:structures.docking-acl, feat:structures.repairshop-service-quotes-and-restoration, rpc:structureSettings.CharacterHasService, rpc:structureSettings.CharacterCheckService
    eve_testkit::attest(&[
        "feat:structures.profiles",
        "feat:structures.docking-acl",
        "feat:structures.repairshop-service-quotes-and-restoration",
        "rpc:structureSettings.CharacterHasService",
        "rpc:structureSettings.CharacterCheckService",
    ]);

    let ctx = new_ctx();
    let owner_corp = 98_902_001;
    let mut owner_sess = setup_corp_ceo(&ctx, 9201, OWNER, owner_corp, "Service ACL Corp", "SACL", SOV_NULL_A, 890_002_001, DVec3::new(1.0e10, 0.0, 0.0)).await;

    let hub_id = 1_000_890_002_100;
    let jb_a_id = 1_000_890_002_201;
    let jb_b_id = 1_000_890_002_202;
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(hub_id, UpwellStructureRecord {
            structure_id: hub_id, type_id: 35833, owner_corp_id: owner_corp, solar_system_id: SOV_NULL_A,
            name: "ACL Service Hub".into(), position: [1.0e10, 0.0, 0.0], state: life::STATE_SHIELD_VULNERABLE.into(),
            quantum_core_installed: true, fuel_blocks: 5_000, shield_hp: 5_000_000.0, armor_hp: 5_000_000.0,
            hull_hp: 5_000_000.0, reinforced_until_unix: 0, last_damage_tick: 0, last_damage_unix: 0, self_destruct_at_unix: None,
        });
        for (idx, (flag, mod_tid)) in [
            (InventoryFlag::ServiceSlot0, 35892_u32), // Standup Market Hub I
            (InventoryFlag::ServiceSlot1, 35899_u32), // Standup Reprocessing Facility I
            (InventoryFlag::ServiceSlot2, 35894_u32), // Standup Cloning Center I
            (InventoryFlag::ServiceSlot3, 35878_u32), // Standup Manufacturing Plant I
        ].into_iter().enumerate() {
            let mid = 890_002_110 + idx as i64;
            snap.items.insert(mid, InventoryItemRecord {
                item_id: mid, type_id: mod_tid, owner_id: owner_corp, location_id: hub_id,
                flag, quantity: 1, singleton: true, version: 1, ..Default::default()
            });
            snap.module_online_states.insert(mid, true);
        }
        for (jid, sys) in [(jb_a_id, SOV_NULL_A), (jb_b_id, SOV_NULL_B)] {
            snap.structures.insert(jid, UpwellStructureRecord {
                structure_id: jid, type_id: 35841, owner_corp_id: owner_corp, solar_system_id: sys,
                name: format!("JB {jid}"), position: [2.0e10, 0.0, 0.0], state: life::STATE_SHIELD_VULNERABLE.into(),
                quantum_core_installed: true, fuel_blocks: 2_000, shield_hp: 1_000_000.0, armor_hp: 1_000_000.0,
                hull_hp: 1_000_000.0, reinforced_until_unix: 0, last_damage_tick: 0, last_damage_unix: 0, self_destruct_at_unix: None,
            });
        }
        snap.items.insert(890_002_120, InventoryItemRecord {
            item_id: 890_002_120, type_id: 16273, owner_id: owner_corp, location_id: jb_a_id,
            flag: InventoryFlag::SpecializedFuelBay, quantity: 50_000, singleton: false, version: 1, ..Default::default()
        });
        let mut ext = eve_server::services::station_structure::structure_upwell_ops::load_ext_state(snap);
        ext.jump_bridge_links.insert(jb_a_id, jb_b_id);
        ext.jump_bridge_links.insert(jb_b_id, jb_a_id);
        eve_server::services::station_structure::structure_upwell_ops::save_ext_state(snap, &ext);
    });

    let grp_dock = kv_i64(&rpc(&ctx, &mut owner_sess, "ownerGroupManager", "Create", &[PyRep::String("Docking ACL".into()), PyRep::String("Can dock".into())]).await.unwrap(), "groupID");
    let grp_vip = kv_i64(&rpc(&ctx, &mut owner_sess, "ownerGroupManager", "Create", &[PyRep::String("VIP Service".into()), PyRep::String("Low tax".into())]).await.unwrap(), "groupID");
    let grp_std = kv_i64(&rpc(&ctx, &mut owner_sess, "ownerGroupManager", "Create", &[PyRep::String("Std Service".into()), PyRep::String("Standard tax".into())]).await.unwrap(), "groupID");

    let mut sess_dock_only = seed_docked_pilot(&ctx, 9202, PILOT_DOCK_ONLY, 98_902_002, None, SOV_NULL_A, hub_id, 890_002_301, 670);
    let mut sess_vip = seed_docked_pilot(&ctx, 9203, PILOT_VIP, 98_902_003, None, SOV_NULL_A, hub_id, 890_002_302, 670);
    let mut sess_std = seed_docked_pilot(&ctx, 9204, PILOT_STD, 98_902_004, None, SOV_NULL_A, hub_id, 890_002_303, 670);
    let mut sess_both = seed_docked_pilot(&ctx, 9205, PILOT_BOTH, 98_902_005, None, SOV_NULL_A, hub_id, 890_002_304, 670);

    rpc(&ctx, &mut owner_sess, "ownerGroupManager", "AddMembers", &[
        PyRep::Integer(grp_dock),
        PyRep::List(vec![PyRep::Integer(PILOT_DOCK_ONLY), PyRep::Integer(PILOT_VIP), PyRep::Integer(PILOT_STD), PyRep::Integer(PILOT_BOTH)]),
    ]).await.unwrap();
    rpc(&ctx, &mut owner_sess, "ownerGroupManager", "AddMembers", &[PyRep::Integer(grp_vip), PyRep::List(vec![PyRep::Integer(PILOT_VIP), PyRep::Integer(PILOT_BOTH)])]).await.unwrap();
    rpc(&ctx, &mut owner_sess, "ownerGroupManager", "AddMembers", &[PyRep::Integer(grp_std), PyRep::List(vec![PyRep::Integer(PILOT_STD), PyRep::Integer(PILOT_BOTH)])]).await.unwrap();

    // Create Profile 1: configure Docking ACL FIRST, then save per-service ACL bindings in a separate SaveProfileSettings call!
    let prof_1 = rpc(&ctx, &mut owner_sess, "structureProfiles", "CreateProfile", &[PyRep::String("Tiered Services".into()), PyRep::String("Tiered".into())]).await.unwrap().as_i64().unwrap();
    rpc(&ctx, &mut owner_sess, "structureProfiles", "SaveProfileSettings", &[
        PyRep::Integer(prof_1),
        PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(svc_acl::SETTING_HOUSING_CAN_DOCK), PyRep::Integer(grp_dock), PyRep::Integer(1)])]),
    ]).await.unwrap();

    // Save per-service ACLs — MUST NOT overwrite grp_dock!
    rpc(&ctx, &mut owner_sess, "structureProfiles", "SaveProfileSettings", &[
        PyRep::Integer(prof_1),
        PyRep::List(vec![
            PyRep::Tuple(vec![PyRep::Integer(svc_acl::SETTING_MARKET_TAX), PyRep::Integer(grp_vip), PyRep::Real(0.01)]),
            PyRep::Tuple(vec![PyRep::Integer(svc_acl::SETTING_MARKET_TAX), PyRep::Integer(grp_std), PyRep::Real(0.05)]),
            PyRep::Tuple(vec![PyRep::Integer(svc_acl::SETTING_REPROCESSING_TAX), PyRep::Integer(grp_vip), PyRep::Real(0.02)]),
            PyRep::Tuple(vec![PyRep::Integer(svc_acl::SETTING_REPROCESSING_TAX), PyRep::Integer(grp_std), PyRep::Real(0.08)]),
            PyRep::Tuple(vec![PyRep::Integer(svc_acl::SETTING_CLONINGBAY_TAX), PyRep::Integer(grp_vip), PyRep::Real(100_000.0)]),
            PyRep::Tuple(vec![PyRep::Integer(svc_acl::SETTING_CLONINGBAY_TAX), PyRep::Integer(grp_std), PyRep::Real(600_000.0)]),
            PyRep::Tuple(vec![PyRep::Integer(svc_acl::SETTING_MANUFACTURING_TAX), PyRep::Integer(grp_vip), PyRep::Real(0.01)]),
            PyRep::Tuple(vec![PyRep::Integer(svc_acl::SETTING_MANUFACTURING_TAX), PyRep::Integer(grp_std), PyRep::Real(0.06)]),
            PyRep::Tuple(vec![PyRep::Integer(svc_acl::SERVICE_REPAIR), PyRep::Integer(grp_vip), PyRep::Real(0.02)]),
            PyRep::Tuple(vec![PyRep::Integer(svc_acl::SERVICE_REPAIR), PyRep::Integer(grp_std), PyRep::Real(0.10)]),
            PyRep::Tuple(vec![PyRep::Integer(svc_acl::SETTING_JUMP_BRIDGE_ACTIVATION), PyRep::Integer(grp_vip), PyRep::Real(0.0)]),
        ]),
    ]).await.unwrap();

    // Verify GetProfileSettings returns real groupID bindings for both Docking (19) and per-service settings!
    let saved_settings = rpc(&ctx, &mut owner_sess, "structureProfiles", "GetProfileSettings", &[PyRep::Integer(prof_1)]).await.unwrap();
    let PyRep::Dict(setting_entries) = saved_settings else { panic!("expected Dict from GetProfileSettings") };
    let groups_for_setting = |sid: i64| -> Vec<(i64, f64)> {
        setting_entries
            .iter()
            .find(|(k, _)| k.as_i64() == Some(sid))
            .and_then(|(_, v)| match v {
                PyRep::List(rows) => Some(
                    rows.iter()
                        .map(|r| {
                            let val = r.dict_get("value").and_then(|x| x.as_f64().or_else(|| x.as_i64().map(|i| i as f64))).unwrap_or(-1.0);
                            (kv_i64(r, "groupID"), val)
                        })
                        .collect(),
                ),
                _ => None,
            })
            .unwrap_or_default()
    };
    assert_eq!(groups_for_setting(svc_acl::SETTING_HOUSING_CAN_DOCK), vec![(grp_dock, 1.0)], "Docking ACL must be preserved");
    assert_eq!(groups_for_setting(svc_acl::SETTING_MARKET_TAX), vec![(grp_vip, 0.01), (grp_std, 0.05)]);
    assert_eq!(groups_for_setting(svc_acl::SETTING_JUMP_BRIDGE_ACTIVATION), vec![(grp_vip, 0.0)]);

    rpc(&ctx, &mut owner_sess, "structureProfiles", "ChangeProfiles", &[PyRep::List(vec![PyRep::Integer(hub_id), PyRep::Integer(jb_a_id)]), PyRep::Integer(prof_1)]).await.unwrap();

    // 1. PILOT_DOCK_ONLY retains Docking access (`CharacterHasSetting(19) == true`), yet is denied every restricted service!
    assert_eq!(
        rpc(&ctx, &mut sess_dock_only, "structureSettings", "CharacterHasSetting", &[PyRep::Integer(hub_id), PyRep::Integer(svc_acl::SETTING_HOUSING_CAN_DOCK)]).await.unwrap(),
        PyRep::Bool(true)
    );
    for svc_id in [svc_acl::SERVICE_MARKET, svc_acl::SERVICE_REPROCESSING, svc_acl::SERVICE_JUMP_CLONE, svc_acl::SERVICE_MANUFACTURING_BASIC] {
        assert_eq!(rpc(&ctx, &mut sess_dock_only, "structureSettings", "CharacterHasService", &[PyRep::Integer(hub_id), PyRep::Integer(svc_id)]).await.unwrap(), PyRep::Bool(false));
        assert_eq!(rpc(&ctx, &mut sess_dock_only, "structureSettings", "CharacterCheckService", &[PyRep::Integer(hub_id), PyRep::Integer(svc_id)]).await.unwrap_err().code, "StructureServiceAccessDenied");
    }

    // Seed ore, Rifter BPO (691), and minerals (34..=40) for all 4 pilots at hub_id.
    ctx.db.with_mut_snapshot(|snap| {
        for (idx, pid) in [PILOT_DOCK_ONLY, PILOT_VIP, PILOT_STD, PILOT_BOTH].into_iter().enumerate() {
            let ore_id = 890_002_400 + idx as i64;
            let bpo_id = 890_002_410 + idx as i64;
            snap.items.insert(ore_id, InventoryItemRecord {
                item_id: ore_id, type_id: 1230, owner_id: pid, location_id: hub_id, flag: InventoryFlag::Hangar, quantity: 10_000, singleton: false, version: 1, ..Default::default()
            });
            snap.items.insert(bpo_id, InventoryItemRecord {
                item_id: bpo_id, type_id: RIFTER_BP as u32, owner_id: pid, location_id: hub_id, flag: InventoryFlag::Hangar, quantity: 1, singleton: true, version: 1, ..Default::default()
            });
            for min_tid in 34..=40_u32 {
                let mid = 890_002_500 + (idx as i64) * 20 + i64::from(min_tid - 34);
                snap.items.insert(mid, InventoryItemRecord {
                    item_id: mid, type_id: min_tid, owner_id: pid, location_id: hub_id, flag: InventoryFlag::Hangar, quantity: 1_000_000, singleton: false, version: 1, ..Default::default()
                });
            }
        }
    });

    // PILOT_DOCK_ONLY is denied Market, Reprocessing, Clone Bay, and Manufacturing RPCs despite being docked!
    let buy_args = [
        PyRep::Integer(hub_id), PyRep::Integer(34), PyRep::Real(100.0), PyRep::Integer(10_000),
        PyRep::Integer(-1), PyRep::Integer(1), PyRep::Integer(90), PyRep::Bool(false),
    ];
    let mfg_args = [PyRep::keyval([
        ("blueprintTypeID", PyRep::Integer(RIFTER_BP)),
        ("facilityID", PyRep::Integer(hub_id)),
        ("activityID", PyRep::Integer(1)),
        ("runs", PyRep::Integer(1)),
    ])];
    assert_eq!(rpc(&ctx, &mut sess_dock_only, "marketProxy", "PlaceBuyOrder", &buy_args).await.unwrap_err().code, "StructureServiceAccessDenied");
    assert_eq!(rpc(&ctx, &mut sess_dock_only, "reprocessingSvc", "Reprocess", &[PyRep::Integer(890_002_400)]).await.unwrap_err().code, "AccessDenied");
    assert_eq!(rpc(&ctx, &mut sess_dock_only, "jumpCloneSvc", "InstallCloneInStructure", &[PyRep::Integer(hub_id)]).await.unwrap_err().code, "StructureAccessDenied");
    assert_eq!(rpc(&ctx, &mut sess_dock_only, "industryManager", "InstallJob", &mfg_args).await.unwrap_err().code, "StructureServiceAccessDenied");
    assert_eq!(rpc(&ctx, &mut sess_dock_only, "repairSvc", "GetRepairQuotes", &[PyRep::List(vec![PyRep::Integer(890_002_301)])]).await.unwrap_err().code, "StructureRepairAccessDenied");

    // 2. Market per-ACL-group broker fees (+0.5% SCC floor): VIP = 1.0% + 0.5% = 15k ISK, STD = 5.0% + 0.5% = 55k ISK, BOTH = min = 15k ISK on 1,000,000 ISK buy escrow!
    let bal_vip_0 = ctx.db.get_character(PILOT_VIP).unwrap().balance.0;
    let bal_std_0 = ctx.db.get_character(PILOT_STD).unwrap().balance.0;
    let bal_both_0 = ctx.db.get_character(PILOT_BOTH).unwrap().balance.0;
    rpc(&ctx, &mut sess_vip, "marketProxy", "PlaceBuyOrder", &buy_args).await.expect("VIP PlaceBuyOrder");
    rpc(&ctx, &mut sess_std, "marketProxy", "PlaceBuyOrder", &buy_args).await.expect("STD PlaceBuyOrder");
    rpc(&ctx, &mut sess_both, "marketProxy", "PlaceBuyOrder", &buy_args).await.expect("BOTH PlaceBuyOrder");
    assert_eq!(bal_vip_0 - ctx.db.get_character(PILOT_VIP).unwrap().balance.0, 1_015_000_00, "VIP must pay 1M escrow + 1.5% (1% owner + 0.5% SCC) broker fee");
    assert_eq!(bal_std_0 - ctx.db.get_character(PILOT_STD).unwrap().balance.0, 1_055_000_00, "STD must pay 1M escrow + 5.5% (5% owner + 0.5% SCC) broker fee");
    assert_eq!(bal_both_0 - ctx.db.get_character(PILOT_BOTH).unwrap().balance.0, 1_015_000_00, "Multi-group pilot must receive lowest matching group rate (1% + 0.5% SCC)");

    // 3. Reprocessing per-ACL-group tax rate: VIP = 2%, STD = 8%!
    let rep_vip = rpc(&ctx, &mut sess_vip, "reprocessingSvc", "Reprocess", &[PyRep::Integer(890_002_401)]).await.expect("VIP Reprocess");
    let rep_std = rpc(&ctx, &mut sess_std, "reprocessingSvc", "Reprocess", &[PyRep::Integer(890_002_402)]).await.expect("STD Reprocess");
    assert_eq!(rep_vip.dict_get("stationTaxRate"), Some(&PyRep::Real(0.02)));
    assert_eq!(rep_std.dict_get("stationTaxRate"), Some(&PyRep::Real(0.08)));

    // 4. Clone Bay per-ACL-group flat ISK fee: VIP = 100,000 ISK, STD = 600,000 ISK!
    let cb_vip_0 = ctx.db.get_character(PILOT_VIP).unwrap().balance.0;
    let cb_std_0 = ctx.db.get_character(PILOT_STD).unwrap().balance.0;
    rpc(&ctx, &mut sess_vip, "jumpCloneSvc", "InstallCloneInStructure", &[PyRep::Integer(hub_id)]).await.expect("VIP InstallClone");
    rpc(&ctx, &mut sess_std, "jumpCloneSvc", "InstallCloneInStructure", &[PyRep::Integer(hub_id)]).await.expect("STD InstallClone");
    assert_eq!(cb_vip_0 - ctx.db.get_character(PILOT_VIP).unwrap().balance.0, 100_000_00);
    assert_eq!(cb_std_0 - ctx.db.get_character(PILOT_STD).unwrap().balance.0, 600_000_00);

    // 5. Manufacturing per-ACL-group facility tax: VIP = 1%, STD = 6%!
    let taxes_vip = rpc(&ctx, &mut sess_vip, "facilityManager", "GetFacilityTaxes", &[PyRep::Integer(hub_id)]).await.unwrap();
    let taxes_std = rpc(&ctx, &mut sess_std, "facilityManager", "GetFacilityTaxes", &[PyRep::Integer(hub_id)]).await.unwrap();
    assert_eq!(taxes_vip.dict_get("1"), Some(&PyRep::Real(0.01)));
    assert_eq!(taxes_std.dict_get("1"), Some(&PyRep::Real(0.06)));
    let job_vip = rpc(&ctx, &mut sess_vip, "industryManager", "InstallJob", &mfg_args).await.expect("VIP InstallJob");
    let job_std = rpc(&ctx, &mut sess_std, "industryManager", "InstallJob", &mfg_args).await.expect("STD InstallJob");
    let vip_ftax = job_vip.dict_get("facilityTaxISK").and_then(|v| v.as_f64()).unwrap();
    let std_ftax = job_std.dict_get("facilityTaxISK").and_then(|v| v.as_f64()).unwrap();
    assert!(std_ftax > vip_ftax * 5.0, "STD 6% facility tax ({std_ftax}) must be ~6x VIP 1% facility tax ({vip_ftax})");

    // 5b. Repairshop per-ACL-group tax rate: VIP = 2%, STD = 10%!
    ctx.db.with_mut_snapshot(|snap| {
        for sh_id in [890_002_302_i64, 890_002_303_i64] {
            let sh = snap.items.get_mut(&sh_id).unwrap();
            sh.type_id = 648; // Badger
            sh.dynamic_attributes.insert(eve_sde::attr_ids::DAMAGE, 500.0);
        }
    });
    let rq_vip = rpc(&ctx, &mut sess_vip, "repairSvc", "GetRepairQuotes", &[PyRep::List(vec![PyRep::Integer(890_002_302)])]).await.expect("VIP GetRepairQuotes");
    let rq_std = rpc(&ctx, &mut sess_std, "repairSvc", "GetRepairQuotes", &[PyRep::List(vec![PyRep::Integer(890_002_303)])]).await.expect("STD GetRepairQuotes");
    let extract_repair_charge = |rep: &PyRep, iid: i64| -> f64 {
        let PyRep::Dict(entries) = rep else { panic!() };
        let PyRep::List(rows) = &entries.iter().find(|(k, _)| k.as_i64() == Some(iid)).unwrap().1 else { panic!() };
        rows[0].dict_get("serviceCharge").and_then(|v| v.as_f64()).unwrap()
    };
    assert!(extract_repair_charge(&rq_std, 890_002_303) > extract_repair_charge(&rq_vip, 890_002_302), "STD 10% repair serviceCharge must exceed VIP 2%");

    // 6. Jump Bridge per-service ACL + 300s delayed ChangeProfiles transition!
    for (sess, char_id, ship_id) in [(&mut sess_vip, PILOT_VIP, 890_002_302_i64), (&mut sess_std, PILOT_STD, 890_002_303_i64)] {
        sess.structure_id = None;
        sess.station_id = None;
        sess.solar_system_id = Some(SOV_NULL_A);
        sess.ship_id = Some(ship_id);
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.structure_id = None;
                ch.solar_system_id = SOV_NULL_A;
                ch.active_ship_id = ship_id;
            }
            if let Some(sh) = snap.items.get_mut(&ship_id) {
                sh.type_id = 587;
                sh.location_id = i64::from(SOV_NULL_A);
                sh.flag = InventoryFlag::None;
            }
        });
        ctx.systems.get_or_spawn(SOV_NULL_A).spawn_ball(Ball {
            id: ship_id, type_id: 587, position: DVec3::new(2.0e10 + 500.0, 0.0, 0.0), velocity: DVec3::ZERO,
            max_velocity: 300.0, inertia: 1.0, mass: 1_000_000.0, radius: 40.0, speed_fraction: 0.0, mode: MovementMode::Stop,
        }).await.unwrap();
    }
    assert_eq!(rpc(&ctx, &mut sess_std, "structureJumpBridgeMgr", "CmdJumpThroughStructureStargate", &[PyRep::Integer(jb_a_id)]).await.unwrap_err().code, "AccessDenied");
    rpc(&ctx, &mut sess_vip, "structureJumpBridgeMgr", "CmdJumpThroughStructureStargate", &[PyRep::Integer(jb_a_id)]).await.expect("VIP pilot jumps Ansiblex");
    assert_eq!(sess_vip.solar_system_id, Some(SOV_NULL_B));

    // Switch jb_a_id to prof_2 (which grants grp_std on SETTING_JUMP_BRIDGE_ACTIVATION) -> 300s delay applies!
    let prof_2 = rpc(&ctx, &mut owner_sess, "structureProfiles", "CreateProfile", &[PyRep::String("Open JB".into()), PyRep::String("JB for Std".into())]).await.unwrap().as_i64().unwrap();
    rpc(&ctx, &mut owner_sess, "structureProfiles", "SaveProfileSettings", &[
        PyRep::Integer(prof_2),
        PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(svc_acl::SETTING_JUMP_BRIDGE_ACTIVATION), PyRep::Real(0.0), PyRep::Integer(grp_std)])]),
    ]).await.unwrap();
    rpc(&ctx, &mut owner_sess, "structureProfiles", "ChangeProfiles", &[PyRep::List(vec![PyRep::Integer(jb_a_id)]), PyRep::Integer(prof_2)]).await.unwrap();

    ctx.advance_virtual_clock(100);
    assert_eq!(rpc(&ctx, &mut sess_std, "structureJumpBridgeMgr", "CmdJumpThroughStructureStargate", &[PyRep::Integer(jb_a_id)]).await.unwrap_err().code, "AccessDenied");
    ctx.advance_virtual_clock(205);
    rpc(&ctx, &mut sess_std, "structureJumpBridgeMgr", "CmdJumpThroughStructureStargate", &[PyRep::Integer(jb_a_id)]).await.expect("STD pilot jumps after 300s delayed profile switch");
    assert_eq!(sess_std.solar_system_id, Some(SOV_NULL_B));
}
