//! End-to-end remediation tests for Structure Directory visibility & ACL filtering
//! (`feat:structures.structure-directory`) and Structure Profile lifecycle / 5-minute transition delay
//! (`feat:structures.profiles`).

use eve_db::{InventoryFlag, InventoryItemRecord, UpwellStructureRecord};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::services::station_structure::structure_lifecycle as life;
use glam::DVec3;
use std::collections::HashMap;
use std::sync::Arc;

const OWNER: i64 = 90_000_001;
const ALLY_PILOT: i64 = 90_000_002;
const PERIMETER: u32 = 30_000_144;
const RANCER: u32 = 30_002_718;

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None).await
}

fn kv_i64(rep: &PyRep, key: &str) -> i64 {
    rep.dict_get(key).and_then(|v| v.as_i64()).unwrap_or(0)
}

fn kv_str(rep: &PyRep, key: &str) -> String {
    rep.dict_get(key).and_then(|v| v.as_str()).unwrap_or("").to_string()
}

fn new_ctx() -> Arc<ServerContext> {
    let ctx = Arc::new(ServerContext::new());
    ctx.set_clock(Arc::new(eve_db::VirtualServerClock::with_fixed_base(1_800_000_000)));
    ctx
}

async fn setup_owner_corp(ctx: &ServerContext, sess_id: u64, sys_id: u32, name: &str, ticker: &str) -> SessionState {
    let station_id = ctx
        .sde
        .stations
        .values()
        .find(|s| s.solar_system_id == sys_id)
        .map(|s| s.station_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&OWNER) {
            let ship_id = ch.active_ship_id;
            ch.solar_system_id = sys_id;
            ch.station_id = station_id;
            if let Some(sh) = snap.items.get_mut(&ship_id) {
                sh.location_id = station_id.unwrap_or(i64::from(sys_id));
            }
        }
    });
    let mut sess = SessionState::new(sess_id);
    rpc(ctx, &mut sess, "authentication", "Login", &[PyRep::String("demo".into()), PyRep::String("demo-secret".into())])
        .await
        .expect("Login");
    rpc(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(OWNER)])
        .await
        .expect("SelectCharacterID");
    ctx.advance_secs(12);
    rpc(
        ctx,
        &mut sess,
        "corpRegistry",
        "AddCorporation",
        &[PyRep::String(name.into()), PyRep::String(ticker.into())],
    )
    .await
    .expect("AddCorporation");
    ctx.advance_virtual_clock(7 * 86_400 + 12);
    if station_id.is_some() {
        rpc(ctx, &mut sess, "structureDocking", "Undock", &[]).await.expect("Undock");
    }
    sess
}

async fn spawn_pilot_ship(
    ctx: &ServerContext,
    sess: &mut SessionState,
    char_id: i64,
    corp_id: i64,
    sys_id: u32,
    ship_id: i64,
    ship_type_id: u32,
    pos: DVec3,
) {
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    sess.solar_system_id = Some(sys_id);
    sess.station_id = None;
    sess.structure_id = None;
    sess.ship_id = Some(ship_id);
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.get(&OWNER).cloned().unwrap();
        ch.character_id = char_id;
        ch.corporation_id = corp_id;
        ch.solar_system_id = sys_id;
        ch.station_id = None;
        ch.structure_id = None;
        ch.active_ship_id = ship_id;
        snap.characters.insert(char_id, ch);
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: ship_type_id,
                owner_id: char_id,
                location_id: i64::from(sys_id),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    let sys = ctx.systems.get_or_spawn(sys_id);
    let _ = sys
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: ship_type_id,
                position: pos,
                velocity: DVec3::ZERO,
                radius: 40.0,
                mass: 1.0e6,
                mode: MovementMode::Stop,
                speed_fraction: 0.0,
                max_velocity: 300.0,
                inertia: 1.0,
            },
            ctx.build_type_combat_state(ship_type_id),
        )
        .await;
    let _ = sys.set_ball_position(ship_id, pos).await;
}

#[tokio::test]
async fn test_remediated_structure_directory_and_profiles_visibility() {
    // parity: feat:structures.structure-directory, feat:structures.profiles
    eve_testkit::attest(&[
        "feat:structures.structure-directory",
        "feat:structures.profiles",
    ]);

    let ctx = new_ctx();
    let mut sess = setup_owner_corp(&ctx, 8230, PERIMETER, "Directory Corp", "DIRC").await;
    let corp_id = sess.corporation_id.expect("corp");
    let st_id = 880_823_001;
    let st_pos = DVec3::new(0.0, 5.0e9, 0.0);

    let group_a = kv_i64(
        &rpc(&ctx, &mut sess, "ownerGroupManager", "Create", &[PyRep::String("Group A".into()), PyRep::String("A".into())])
            .await
            .unwrap(),
        "groupID",
    );
    rpc(&ctx, &mut sess, "ownerGroupManager", "AddMembers", &[PyRep::Integer(group_a), PyRep::List(vec![PyRep::Integer(ALLY_PILOT)])])
        .await
        .unwrap();

    let prof_a = rpc(&ctx, &mut sess, "structureProfiles", "CreateProfile", &[PyRep::String("Profile A".into()), PyRep::String("Initial".into())])
        .await
        .unwrap()
        .as_i64()
        .unwrap();
    let prof_b = rpc(&ctx, &mut sess, "structureProfiles", "CreateProfile", &[PyRep::String("Profile B".into()), PyRep::String("Delayed".into())])
        .await
        .unwrap()
        .as_i64()
        .unwrap();
    rpc(&ctx, &mut sess, "structureProfiles", "UpdateProfile", &[PyRep::Integer(prof_a), PyRep::String("Profile A Renamed".into()), PyRep::String("Updated desc".into())])
        .await
        .unwrap();
    rpc(&ctx, &mut sess, "structureProfiles", "SetDefaultProfile", &[PyRep::Integer(prof_a)])
        .await
        .unwrap();
    let dup_id = rpc(&ctx, &mut sess, "structureProfiles", "DuplicateProfile", &[PyRep::Integer(prof_a)])
        .await
        .unwrap()
        .as_i64()
        .unwrap();
    let profiles_list = rpc(&ctx, &mut sess, "structureProfiles", "GetProfiles", &[]).await.unwrap();
    let PyRep::List(pl) = &profiles_list else { panic!("expected list from GetProfiles") };
    assert_eq!(pl.len(), 3);
    let pa_entry = pl.iter().find(|p| kv_i64(p, "profileID") == prof_a).expect("prof_a in GetProfiles");
    assert_eq!(kv_str(pa_entry, "name"), "Profile A Renamed");
    assert_eq!(pa_entry.dict_get("isDefault"), Some(&PyRep::Bool(true)));
    rpc(&ctx, &mut sess, "structureProfiles", "DeleteProfile", &[PyRep::Integer(dup_id)])
        .await
        .expect("DeleteProfile on unused duplicate");

    rpc(
        &ctx,
        &mut sess,
        "structureProfiles",
        "SaveProfileSettings",
        &[
            PyRep::Integer(prof_a),
            PyRep::Dict(vec![
                (PyRep::String("corpOnly".into()), PyRep::Bool(false)),
                (PyRep::String("accessGroups".into()), PyRep::List(vec![PyRep::Integer(group_a)])),
                (PyRep::String("services".into()), PyRep::Dict(vec![(PyRep::Integer(2), PyRep::Real(0.015))])),
            ]),
        ],
    )
    .await
    .unwrap();
    let settings_rep = rpc(&ctx, &mut sess, "structureProfiles", "GetProfileSettings", &[PyRep::Integer(prof_a)])
        .await
        .expect("GetProfileSettings");
    let PyRep::Dict(sdict) = settings_rep else { panic!("expected dict from GetProfileSettings") };
    assert!(sdict.iter().any(|(k, _)| k.as_i64() == Some(1)), "GetProfileSettings must include docking setting 1");
    assert!(sdict.iter().any(|(k, _)| k.as_i64() == Some(2)), "GetProfileSettings must include market service setting 2");

    // Partial SaveProfileSettings with V24 settingID 17 and raw character ID >= 90,000,001 preserves omitted settings.
    rpc(
        &ctx,
        &mut sess,
        "structureProfiles",
        "SaveProfileSettings",
        &[
            PyRep::Integer(prof_a),
            PyRep::Dict(vec![(PyRep::Integer(17), PyRep::List(vec![PyRep::Integer(ALLY_PILOT)]))]),
        ],
    )
    .await
    .unwrap();
    let PyRep::Dict(sdict17) = rpc(&ctx, &mut sess, "structureProfiles", "GetProfileSettings", &[PyRep::Integer(prof_a)])
        .await
        .unwrap()
    else {
        panic!("expected dict");
    };
    let PyRep::List(s17_rows) = sdict17.iter().find(|(k, _)| k.as_i64() == Some(17)).map(|(_, v)| v).unwrap() else {
        panic!("expected list for setting 17");
    };
    assert!(
        s17_rows.iter().any(|r| r.dict_get("groupID").and_then(|v| v.as_i64()) == Some(ALLY_PILOT)),
        "Setting 17 must store raw character ID >= 90,000,001"
    );
    assert!(sdict17.iter().any(|(k, _)| k.as_i64() == Some(1)), "Partial update must preserve setting 1");
    assert!(sdict17.iter().any(|(k, _)| k.as_i64() == Some(2)), "Partial update must preserve setting 2");
    rpc(
        &ctx,
        &mut sess,
        "structureProfiles",
        "SaveProfileSettings",
        &[PyRep::Integer(prof_a), PyRep::Dict(vec![(PyRep::Integer(17), PyRep::List(Vec::new()))])],
    )
    .await
    .unwrap();

    rpc(
        &ctx,
        &mut sess,
        "structureProfiles",
        "SaveProfileSettings",
        &[
            PyRep::Integer(prof_b),
            PyRep::Dict(vec![
                (PyRep::String("corpOnly".into()), PyRep::Bool(true)),
                (PyRep::String("services".into()), PyRep::Dict(vec![(PyRep::Integer(2), PyRep::Real(0.04))])),
            ]),
        ],
    )
    .await
    .unwrap();

    let jb_a_id = 880_823_002;
    let jb_b_id = 880_823_003;
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            st_id,
            UpwellStructureRecord {
                structure_id: st_id,
                type_id: 35832,
                owner_corp_id: corp_id,
                solar_system_id: PERIMETER,
                name: "Directory Astrahus".into(),
                state: life::STATE_SHIELD_VULNERABLE.into(),
                position: st_pos.to_array(),
                quantum_core_installed: true,
                fuel_blocks: 1_200,
                shield_hp: 1_000_000.0,
                armor_hp: 1_000_000.0,
                hull_hp: 1_000_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
        for (jbid, sys) in [(jb_a_id, PERIMETER), (jb_b_id, RANCER)] {
            snap.structures.insert(
                jbid,
                UpwellStructureRecord {
                    structure_id: jbid,
                    type_id: 35841,
                    owner_corp_id: corp_id,
                    solar_system_id: sys,
                    name: format!("JB {jbid}"),
                    state: life::STATE_SHIELD_VULNERABLE.into(),
                    position: (st_pos + DVec3::new(200_000_000.0, 0.0, 0.0)).to_array(),
                    quantum_core_installed: false,
                    fuel_blocks: 1_200,
                    shield_hp: 1_000_000.0,
                    armor_hp: 1_000_000.0,
                    hull_hp: 1_000_000.0,
                    reinforced_until_unix: 0,
                    last_damage_tick: 0,
                    last_damage_unix: 0,
                    self_destruct_at_unix: None,
                },
            );
        }
        let mut ext = eve_server::services::station_structure::structure_upwell_ops::load_ext_state(snap);
        ext.jump_bridge_links.insert(jb_a_id, jb_b_id);
        ext.jump_bridge_links.insert(jb_b_id, jb_a_id);
        eve_server::services::station_structure::structure_upwell_ops::save_ext_state(snap, &ext);
        let svc_id = 880_823_050;
        snap.items.insert(
            svc_id,
            InventoryItemRecord {
                item_id: svc_id,
                type_id: 35892, // Standup Market Hub I
                owner_id: corp_id,
                location_id: st_id,
                flag: InventoryFlag::ServiceSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(svc_id, true);
    });
    life::sync_all_structures(&ctx);

    // Initial profile assignment applies immediately; switching to Profile B queues a 5-minute (300s) delay!
    rpc(
        &ctx,
        &mut sess,
        "structureProfiles",
        "ChangeProfiles",
        &[PyRep::List(vec![PyRep::Integer(st_id), PyRep::Integer(jb_a_id)]), PyRep::Integer(prof_a)],
    )
    .await
    .unwrap();
    assert_eq!(
        rpc(&ctx, &mut sess, "structureProfiles", "DeleteProfile", &[PyRep::Integer(prof_a)])
            .await
            .expect_err("cannot delete profile in use")
            .code,
        "ProfileInUse"
    );
    rpc(
        &ctx,
        &mut sess,
        "structureProfiles",
        "ChangeProfiles",
        &[PyRep::List(vec![PyRep::Integer(st_id), PyRep::Integer(jb_a_id)]), PyRep::Integer(prof_b)],
    )
    .await
    .unwrap();

    let mut ally_sess = SessionState::new(8231);
    ally_sess.user_id = Some(2);
    spawn_pilot_ship(&ctx, &mut ally_sess, ALLY_PILOT, 98_820_002, PERIMETER, 880_823_011, 587, st_pos + DVec3::new(1_000.0, 0.0, 0.0)).await;

    // At t + 299s: Profile A is still active -> ally sees structure in GetStructures and GetJumpBridgesWithMyAccess returns V24 3-tuple.
    ctx.advance_virtual_clock(299);
    let PyRep::Tuple(jb_before) = rpc(&ctx, &mut ally_sess, "structureDirectory", "GetJumpBridgesWithMyAccess", &[]).await.unwrap() else {
        panic!("expected 3-tuple from GetJumpBridgesWithMyAccess");
    };
    assert_eq!(jb_before.len(), 3);
    let PyRep::List(jb_before_ids) = &jb_before[1] else { panic!("expected list at tuple[1]") };
    assert!(jb_before_ids.contains(&PyRep::Integer(jb_a_id)), "Ally must see jb_a_id before delayed profile switch completes");
    let corp_structs = rpc(&ctx, &mut sess, "structureDirectory", "GetCorporationStructures", &[]).await.unwrap();
    let PyRep::Dict(cs_map) = corp_structs else { panic!("expected dict") };
    let cs_entry = cs_map.iter().find(|(k, _)| k.as_i64() == Some(st_id)).map(|(_, v)| v).expect("corp structure");
    assert_eq!(kv_i64(cs_entry, "profileID"), prof_a);
    assert_eq!(kv_str(cs_entry, "powerState"), "FullPower");

    let ally_dir = rpc(&ctx, &mut ally_sess, "structureDirectory", "GetStructures", &[]).await.unwrap();
    let PyRep::Dict(ad_map) = ally_dir else { panic!("expected dict") };
    let ad_entry = ad_map.iter().find(|(k, _)| k.as_i64() == Some(st_id)).map(|(_, v)| v).expect("ally sees structure");
    assert_eq!(ad_entry.dict_get("fuelBlocks"), Some(&PyRep::None), "Non-corp pilot must not see exact fuelBlocks");
    assert_eq!(ad_entry.dict_get("profileID"), Some(&PyRep::None), "External pilot must not see profileID in GetStructures");

    let ally_info = rpc(&ctx, &mut ally_sess, "structureDirectory", "GetStructureInfo", &[PyRep::Integer(st_id)]).await.unwrap();
    assert_eq!(ally_info.dict_get("profileID"), Some(&PyRep::None), "External pilot must not see profileID in GetStructureInfo");
    let owner_info = rpc(&ctx, &mut sess, "structureDirectory", "GetStructureInfo", &[PyRep::Integer(st_id)]).await.unwrap();
    assert_eq!(kv_i64(&owner_info, "profileID"), prof_a, "Owning corp manager must see profileID in GetStructureInfo");

    // At t + 300s: Profile B (corpOnly = true) activates -> remote ally is denied GetStructureInfo and excluded from GetJumpBridgesWithMyAccess!
    ctx.advance_virtual_clock(1);
    let PyRep::Tuple(jb_after) = rpc(&ctx, &mut ally_sess, "structureDirectory", "GetJumpBridgesWithMyAccess", &[]).await.unwrap() else {
        panic!("expected 3-tuple");
    };
    let PyRep::List(jb_after_ids) = &jb_after[1] else { panic!("expected list") };
    assert!(!jb_after_ids.contains(&PyRep::Integer(jb_a_id)), "Ally must lose jb_a_id access at t + 300s");
    ally_sess.solar_system_id = Some(RANCER);
    ctx.db.with_mut_snapshot(|snap| snap.characters.get_mut(&ALLY_PILOT).unwrap().solar_system_id = RANCER);
    assert_eq!(
        rpc(&ctx, &mut ally_sess, "structureDirectory", "GetStructureInfo", &[PyRep::Integer(st_id)])
            .await
            .expect_err("Profile B corpOnly blocks remote ally")
            .code,
        "StructureAccessDenied"
    );
    let updated_cs = rpc(&ctx, &mut sess, "structureDirectory", "GetCorporationStructures", &[]).await.unwrap();
    let PyRep::Dict(ucs_map) = updated_cs else { panic!("expected dict") };
    let ucs_entry = ucs_map.iter().find(|(k, _)| k.as_i64() == Some(st_id)).map(|(_, v)| v).expect("corp structure");
    assert_eq!(kv_i64(ucs_entry, "profileID"), prof_b);
}

#[tokio::test]
async fn test_remediated_structure_control_release_and_flex_zero_arg_release() {
    // parity: feat:structures.board-structure, feat:structures.defense-settings, rpc:structureControl.TakeControl, rpc:structureControl.ReleaseControl, rpc:structureControl.GetStructurePilot
    eve_testkit::attest(&[
        "feat:structures.board-structure",
        "feat:structures.defense-settings",
        "rpc:structureControl.TakeControl",
        "rpc:structureControl.ReleaseControl",
        "rpc:structureControl.GetStructurePilot",
    ]);

    let ctx = new_ctx();
    let mut owner_sess = setup_owner_corp(&ctx, 8240, RANCER, "Control Release Corp", "CREL").await;
    let corp_id = owner_sess.corporation_id.expect("corp");
    let owner_orig_ship = owner_sess.ship_id.expect("owner ship");
    let st_id = 880_824_001;
    let flex_id = 880_824_002;
    let dir2: i64 = 90_000_041;
    let dir3: i64 = 90_000_042;
    let gunner: i64 = 90_000_043;
    let dir2_ship = 880_824_012;
    let dir3_ship = 880_824_013;
    let gunner_ship = 880_824_014;
    let st_pos = DVec3::new(0.0, 5.0e9, 0.0);
    let flex_pos = DVec3::new(1.0e9, 5.0e9, 0.0);

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            st_id,
            UpwellStructureRecord {
                structure_id: st_id,
                type_id: 35832,
                owner_corp_id: corp_id,
                solar_system_id: RANCER,
                name: "Control Citadel".into(),
                state: life::STATE_SHIELD_VULNERABLE.into(),
                position: st_pos.to_array(),
                quantum_core_installed: true,
                fuel_blocks: 1_200,
                shield_hp: 1_000_000.0,
                armor_hp: 1_000_000.0,
                hull_hp: 1_000_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
        snap.structures.insert(
            flex_id,
            UpwellStructureRecord {
                structure_id: flex_id,
                type_id: 35841, // Ansiblex Jump Bridge (FLEX)
                owner_corp_id: corp_id,
                solar_system_id: RANCER,
                name: "Control Ansiblex".into(),
                state: life::STATE_SHIELD_VULNERABLE.into(),
                position: flex_pos.to_array(),
                quantum_core_installed: false,
                fuel_blocks: 1_200,
                shield_hp: 1_000_000.0,
                armor_hp: 1_000_000.0,
                hull_hp: 1_000_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
        for (cid, ccorp, shid, is_dir) in [
            (dir2, corp_id, dir2_ship, true),
            (dir3, corp_id, dir3_ship, true),
            (gunner, 98_824_099, gunner_ship, false),
        ] {
            let mut ch = snap.characters.get(&OWNER).cloned().unwrap();
            ch.character_id = cid;
            ch.corporation_id = ccorp;
            ch.solar_system_id = RANCER;
            ch.structure_id = Some(st_id);
            ch.station_id = None;
            ch.active_ship_id = shid;
            snap.characters.insert(cid, ch);
            snap.items.insert(
                shid,
                InventoryItemRecord {
                    item_id: shid,
                    type_id: 587,
                    owner_id: cid,
                    location_id: st_id,
                    flag: InventoryFlag::Hangar,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
            if is_dir {
                if let Some(corp) = snap.corporations.get_mut(&corp_id) {
                    corp.members.insert(cid);
                    corp.member_roles.insert(cid, eve_db::corp_roles::ROLE_DIRECTOR);
                }
            }
        }
    });
    life::sync_all_structures(&ctx);

    let dock_session = |sid: u64, uid: u64, cid: i64, ccorp: i64, role: u64, shid: i64| {
        let mut s = SessionState::new(sid);
        s.user_id = Some(uid);
        s.character_id = Some(cid);
        s.corporation_id = Some(ccorp);
        s.corp_role = role;
        s.solar_system_id = Some(RANCER);
        s.structure_id = Some(st_id);
        s.ship_id = Some(shid);
        s
    };

    // 1. Logout releases control & restores persisted active_ship_id so second director can TakeControl.
    let rancer_sys = ctx.systems.get_or_spawn(RANCER);
    assert!(rancer_sys.set_ball_position(owner_orig_ship, st_pos + DVec3::new(100.0, 0.0, 0.0)).await);
    rpc(&ctx, &mut owner_sess, "stationSvc", "Dock", &[PyRep::Integer(st_id)]).await.expect("owner dock");
    rpc(&ctx, &mut owner_sess, "structureControl", "TakeControl", &[PyRep::Integer(st_id)]).await.expect("owner TakeControl");
    assert_eq!(ctx.db.with_snapshot(|s| s.characters[&OWNER].active_ship_id), st_id);

    let mut dir2_sess = dock_session(8241, 2, dir2, corp_id, eve_db::corp_roles::ROLE_DIRECTOR, dir2_ship);
    assert_eq!(
        rpc(&ctx, &mut dir2_sess, "structureControl", "TakeControl", &[PyRep::Integer(st_id)])
            .await
            .expect_err("second director blocked while owner controlling")
            .code,
        "StructureAlreadyControlled"
    );

    rpc(&ctx, &mut owner_sess, "authentication", "Logout", &[]).await.expect("Logout");
    assert_eq!(ctx.db.with_snapshot(|s| s.characters[&OWNER].active_ship_id), owner_orig_ship, "Logout must restore persisted active_ship_id");
    assert_eq!(
        rpc(&ctx, &mut dir2_sess, "structureControl", "GetStructurePilot", &[PyRep::Integer(st_id)]).await.unwrap(),
        PyRep::None
    );

    // 2. Corporation change (LeaveCorp via KickOutMembers self) releases control & restores active_ship_id.
    rpc(&ctx, &mut dir2_sess, "structureControl", "TakeControl", &[PyRep::Integer(st_id)]).await.expect("dir2 TakeControl");
    assert_eq!(ctx.db.with_snapshot(|s| s.characters[&dir2].active_ship_id), st_id);
    ctx.advance_secs(12);
    rpc(&ctx, &mut dir2_sess, "corpRegistry", "KickOutMembers", &[PyRep::Integer(dir2)]).await.expect("dir2 LeaveCorp");
    assert_eq!(ctx.db.with_snapshot(|s| s.characters[&dir2].active_ship_id), dir2_ship, "Leaving corp must restore active_ship_id");
    assert_eq!(dir2_sess.ship_id, Some(dir2_ship));

    // 3. Role loss (UpdateMember stripping ROLE_DIRECTOR) releases control & restores active_ship_id.
    let mut dir3_sess = dock_session(8242, 3, dir3, corp_id, eve_db::corp_roles::ROLE_DIRECTOR, dir3_ship);
    rpc(&ctx, &mut dir3_sess, "structureControl", "TakeControl", &[PyRep::Integer(st_id)]).await.expect("dir3 TakeControl");
    assert_eq!(ctx.db.with_snapshot(|s| s.characters[&dir3].active_ship_id), st_id);

    // Log OWNER back in so OWNER can strip dir3's director role.
    let mut owner_relogin = SessionState::new(8243);
    rpc(&ctx, &mut owner_relogin, "authentication", "Login", &[PyRep::String("demo".into()), PyRep::String("demo-secret".into())]).await.unwrap();
    rpc(&ctx, &mut owner_relogin, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(OWNER)]).await.unwrap();
    rpc(&ctx, &mut owner_relogin, "corpRegistry", "UpdateMember", &[PyRep::Integer(dir3), PyRep::Integer(0)]).await.expect("strip dir3 roles");
    assert_eq!(ctx.db.with_snapshot(|s| s.characters[&dir3].active_ship_id), dir3_ship, "Role loss must restore active_ship_id");
    ctx.sync_session(&mut dir3_sess);
    assert_eq!(dir3_sess.ship_id, Some(dir3_ship), "Role loss must synchronize live SessionState.ship_id");

    // 4. ACL loss (removing gunner from defense access group) releases control & restores active_ship_id.
    let def_grp = kv_i64(
        &rpc(&ctx, &mut owner_relogin, "ownerGroupManager", "Create", &[PyRep::String("Defense Gunners".into()), PyRep::String("Gunners".into())]).await.unwrap(),
        "groupID",
    );
    rpc(&ctx, &mut owner_relogin, "ownerGroupManager", "AddMembers", &[PyRep::Integer(def_grp), PyRep::List(vec![PyRep::Integer(gunner)])]).await.unwrap();
    let def_prof = rpc(&ctx, &mut owner_relogin, "structureProfiles", "CreateProfile", &[PyRep::String("Gunner ACL Profile".into()), PyRep::String("Defense ACL".into())]).await.unwrap().as_i64().unwrap();
    rpc(
        &ctx,
        &mut owner_relogin,
        "structureProfiles",
        "SaveProfileSettings",
        &[
            PyRep::Integer(def_prof),
            PyRep::Dict(vec![
                (PyRep::String("corpOnly".into()), PyRep::Bool(false)),
                (PyRep::String("defenseGroupIDs".into()), PyRep::List(vec![PyRep::Integer(def_grp)])),
            ]),
        ],
    )
    .await
    .unwrap();
    rpc(&ctx, &mut owner_relogin, "structureProfiles", "ChangeProfiles", &[PyRep::List(vec![PyRep::Integer(st_id)]), PyRep::Integer(def_prof)]).await.unwrap();

    let mut gunner_sess = dock_session(8244, 4, gunner, 98_824_099, 0, gunner_ship);
    rpc(&ctx, &mut gunner_sess, "structureControl", "TakeControl", &[PyRep::Integer(st_id)]).await.expect("gunner TakeControl via defense ACL");
    assert_eq!(ctx.db.with_snapshot(|s| s.characters[&gunner].active_ship_id), st_id);

    rpc(&ctx, &mut owner_relogin, "ownerGroupManager", "DeleteMembers", &[PyRep::Integer(def_grp), PyRep::List(vec![PyRep::Integer(gunner)])]).await.expect("remove gunner from defense ACL");
    assert_eq!(ctx.db.with_snapshot(|s| s.characters[&gunner].active_ship_id), gunner_ship, "ACL loss must restore active_ship_id");
    ctx.sync_session(&mut gunner_sess);
    assert_eq!(gunner_sess.ship_id, Some(gunner_ship), "ACL loss must synchronize live SessionState.ship_id");

    // 5. Abrupt disconnect releases control & restores active_ship_id.
    rpc(&ctx, &mut owner_relogin, "structureControl", "TakeControl", &[PyRep::Integer(st_id)]).await.expect("owner TakeControl before disconnect");
    assert_eq!(ctx.db.with_snapshot(|s| s.characters[&OWNER].active_ship_id), st_id);
    eve_server::services::fleet_social::fleet_social_notify::handle_character_disconnect(&ctx, OWNER);
    assert_eq!(ctx.db.with_snapshot(|s| s.characters[&OWNER].active_ship_id), owner_orig_ship, "Disconnect must restore active_ship_id");

    // 6. 0-arg ReleaseControl() on a FLEX structure in space (session.structure_id == None) succeeds & restores ship_id!
    rpc(&ctx, &mut owner_relogin, "authentication", "Logout", &[]).await.unwrap();
    let mut flex_sess = SessionState::new(8245);
    rpc(&ctx, &mut flex_sess, "authentication", "Login", &[PyRep::String("demo".into()), PyRep::String("demo-secret".into())]).await.unwrap();
    rpc(&ctx, &mut flex_sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(OWNER)]).await.unwrap();
    ctx.advance_secs(12);
    rpc(&ctx, &mut flex_sess, "structureDocking", "Undock", &[]).await.expect("undock for FLEX test");
    assert!(rancer_sys.set_ball_position(owner_orig_ship, flex_pos + DVec3::new(500.0, 0.0, 0.0)).await);
    rpc(&ctx, &mut flex_sess, "structureControl", "TakeControl", &[PyRep::Integer(flex_id)]).await.expect("TakeControl on FLEX");
    assert_eq!(flex_sess.ship_id, Some(flex_id));
    rpc(&ctx, &mut flex_sess, "structureControl", "ReleaseControl", &[]).await.expect("0-arg ReleaseControl on FLEX");
    assert_eq!(flex_sess.ship_id, Some(owner_orig_ship), "0-arg ReleaseControl on FLEX must restore session.ship_id");
    assert_eq!(ctx.db.with_snapshot(|s| s.characters[&OWNER].active_ship_id), owner_orig_ship);

    // 7. Standing direction: pilot->corp negative standing is NOT auto-targeted, whereas corp->pilot negative standing IS auto-targeted!
    ctx.db.with_mut_snapshot(|snap| {
        let mut ds = eve_server::services::structure_acl_defense_rules::load_defense_settings(snap, st_id);
        ds.auto_defense_enabled = true;
        ds.standing_threshold = -1.0;
        ds.target_blocked_acl = false;
        eve_server::services::structure_acl_defense_rules::save_defense_settings(snap, &ds);
        if let Some(ch) = snap.characters.get_mut(&gunner) {
            ch.standings.insert(corp_id, -10.0);
        }
    });
    let pilot_to_corp_targeted = ctx.db.with_snapshot(|snap| {
        eve_server::services::structure_acl_defense_rules::evaluate_auto_defense_target(&ctx, snap, st_id, gunner)
    });
    assert!(!pilot_to_corp_targeted, "Pilot->corp standing must NOT trigger structure auto-defense");
    ctx.db.with_mut_snapshot(|snap| {
        snap.corp_contacts.entry(corp_id).or_default().push(eve_db::SocialContactRecord {
            contact_id: gunner,
            contact_type: "character".into(),
            standing: -5.0,
            is_watched: false,
            is_blocked: false,
            label_ids: Vec::new(),
        });
    });
    let corp_to_pilot_targeted = ctx.db.with_snapshot(|snap| {
        eve_server::services::structure_acl_defense_rules::evaluate_auto_defense_target(&ctx, snap, st_id, gunner)
    });
    assert!(corp_to_pilot_targeted, "Corp->pilot standing < standingThreshold MUST trigger structure auto-defense");
}
