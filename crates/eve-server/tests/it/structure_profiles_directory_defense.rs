//! End-to-end V24 RPC integration tests for Upwell structure profiles, structure directory
//! visibility & sensitive masking, and automated grid defense retaliation (`feat:structures.profiles`,
//! `feat:structures.structure-directory`, `feat:structures.defense-settings`).

use eve_db::{InventoryFlag, InventoryItemRecord, UpwellStructureRecord};
use eve_destiny::{Ball, MovementMode, TargetLock};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::services::station_structure::structure_lifecycle as life;
use eve_server::services::structure_acl_defense_rules as acl_rules;
use glam::DVec3;
use std::collections::HashMap;

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let kw = HashMap::new();
    ctx.dispatch_rpc(sess, service, None, method, args, &kw, None)
        .await
}

fn kv_entry<'a>(rep: &'a PyRep, key: &str) -> Option<&'a PyRep> {
    match rep {
        PyRep::KeyVal(entries) => entries.get(key),
        PyRep::Dict(entries) => entries.iter().find(|(k, _)| k.as_str() == Some(key)).map(|(_, v)| v),
        _ => None,
    }
}

async fn login_with_corp(ctx: &ServerContext, sess_id: u64, char_id: i64, ticker: &str) -> SessionState {
    let mut sess = SessionState::new(sess_id);
    rpc(ctx, &mut sess, "authentication", "Login", &[PyRep::String("demo".into()), PyRep::String("demo-secret".into())])
        .await
        .expect("Login");
    rpc(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)])
        .await
        .expect("SelectCharacterID");
    ctx.advance_secs(12);
    let corp_name = format!("Profile Sovereign {char_id}");
    rpc(ctx, &mut sess, "corpRegistry", "AddCorporation", &[PyRep::String(corp_name.into()), PyRep::String(ticker.into())])
        .await
        .expect("AddCorporation");
    sess.corp_role |= eve_db::corp_roles::ROLE_DIRECTOR | eve_db::corp_roles::ROLE_STATION_MANAGER;
    ctx.advance_secs(7 * 86_400 + 25);
    sess
}

fn group_id_of(rep: &PyRep) -> i64 {
    kv_entry(rep, "groupID").and_then(|v| v.as_i64()).or_else(|| rep.as_i64()).expect("groupID")
}

fn seed_operational_structure(
    ctx: &ServerContext,
    structure_id: i64,
    type_id: u32,
    corp_id: i64,
    sys_id: u32,
    pos: [f64; 3],
) {
    let profile = life::structure_profile(ctx, type_id).expect("valid structure type");
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            structure_id,
            UpwellStructureRecord {
                structure_id,
                type_id,
                owner_corp_id: corp_id,
                solar_system_id: sys_id,
                position: pos,
                state: life::STATE_SHIELD_VULNERABLE.to_string(),
                quantum_core_installed: true,
                fuel_blocks: 2_400,
                shield_hp: profile.shield_hp,
                armor_hp: profile.armor_hp,
                hull_hp: profile.hull_hp,
                name: format!("Upwell {structure_id}"),
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
        snap.structure_vulnerability_windows.insert(structure_id, (18, 5));
        snap.items.insert(
            structure_id,
            InventoryItemRecord {
                item_id: structure_id,
                type_id,
                owner_id: corp_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
}

#[tokio::test]
async fn test_structure_profiles_crud_service_acl_and_delayed_change() {
    // parity: feat:structures.profiles, feat:structures.docking-acl
    eve_testkit::attest(&[
        "feat:structures.profiles",
        "feat:structures.docking-acl",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);

    let dir_char = 90_000_001;
    let guest_dock_only = 90_000_002;
    let guest_reproc = 90_000_004;
    let unpriv_corp_member = 90_000_005;

    let mut dir_sess = login_with_corp(&ctx, 501, dir_char, "PRFL").await;
    let corp_id = ctx.db.with_snapshot(|s| s.characters[&dir_char].corporation_id);

    ctx.db.with_mut_snapshot(|snap| {
        let base = snap.characters[&guest_dock_only].clone();
        let mut cr = base.clone();
        cr.character_id = guest_reproc;
        cr.corporation_id = 98_000_040;
        cr.name = "Reprocessing Partner".into();
        snap.characters.insert(guest_reproc, cr);

        let mut cm = base;
        cm.character_id = unpriv_corp_member;
        cm.corporation_id = corp_id;
        cm.name = "Line Member".into();
        snap.characters.insert(unpriv_corp_member, cm);
    });

    // 1. Non-manager corp member cannot create or edit profiles
    let mut line_sess = SessionState::new(502);
    line_sess.user_id = Some(5);
    line_sess.character_id = Some(unpriv_corp_member);
    line_sess.corporation_id = Some(corp_id);
    line_sess.corp_role = 0;
    assert_eq!(
        rpc(&ctx, &mut line_sess, "structureProfiles", "CreateProfile", &[PyRep::String("Forbidden".into()), PyRep::String("".into())])
            .await
            .unwrap_err()
            .code,
        "InsufficientCorpRoles"
    );

    // 2. Create two ACLs: dock_acl (both guests) and reproc_acl (only guest_reproc)
    let dock_acl = group_id_of(&rpc(&ctx, &mut dir_sess, "ownerGroupManager", "Create", &[PyRep::String("Dock Guests".into()), PyRep::String("".into())]).await.unwrap());
    let reproc_acl = group_id_of(&rpc(&ctx, &mut dir_sess, "ownerGroupManager", "Create", &[PyRep::String("Reproc VIP".into()), PyRep::String("".into())]).await.unwrap());
    rpc(&ctx, &mut dir_sess, "ownerGroupManager", "UpdateMembershipTypes", &[PyRep::Integer(dock_acl), PyRep::List(vec![PyRep::Integer(guest_dock_only), PyRep::Integer(guest_reproc)]), PyRep::Integer(acl_rules::ACL_ROLE_MEMBER)])
        .await.unwrap();
    rpc(&ctx, &mut dir_sess, "ownerGroupManager", "UpdateMembershipTypes", &[PyRep::Integer(reproc_acl), PyRep::List(vec![PyRep::Integer(guest_reproc)]), PyRep::Integer(acl_rules::ACL_ROLE_MEMBER)])
        .await.unwrap();

    // 3. CreateProfile, UpdateProfile, DuplicateProfile, SetDefaultProfile, GetProfiles
    let prof_a = rpc(&ctx, &mut dir_sess, "structureProfiles", "CreateProfile", &[PyRep::String("Industrial Hub".into()), PyRep::String("Initial".into())])
        .await.unwrap().as_i64().unwrap();
    rpc(&ctx, &mut dir_sess, "structureProfiles", "UpdateProfile", &[PyRep::Integer(prof_a), PyRep::String("Industrial Hub V2".into()), PyRep::String("Updated".into())])
        .await.unwrap();

    // Save per-service ACLs and tax rate (setting 3 = Reprocessing at 3.5% tax = 0.035; setting 2 = Clone Bay restricted to reproc_acl)
    assert_eq!(
        rpc(&ctx, &mut dir_sess, "structureProfiles", "SaveProfileSettings", &[PyRep::Integer(prof_a), PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(3), PyRep::Real(1.5), PyRep::Integer(reproc_acl)])])]).await.unwrap_err().code,
        "InvalidStructureTaxRate"
    );
    rpc(
        &ctx,
        &mut dir_sess,
        "structureProfiles",
        "SaveProfileSettings",
        &[
            PyRep::Integer(prof_a),
            PyRep::List(vec![
                PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Real(0.0), PyRep::Integer(dock_acl)]),
                PyRep::Tuple(vec![PyRep::Integer(3), PyRep::Real(0.035), PyRep::Integer(reproc_acl)]),
                PyRep::Tuple(vec![PyRep::Integer(23), PyRep::Real(50_000.0), PyRep::Integer(reproc_acl)]),
            ]),
        ],
    )
    .await
    .expect("SaveProfileSettings");

    let settings_rep = rpc(&ctx, &mut dir_sess, "structureProfiles", "GetProfileSettings", &[PyRep::Integer(prof_a)])
        .await
        .expect("GetProfileSettings");
    let PyRep::Dict(setting_groups) = settings_rep else { panic!("expected dict from GetProfileSettings") };
    assert!(setting_groups.iter().any(|(k, _)| k.as_i64() == Some(3)));

    let dup_prof = rpc(&ctx, &mut dir_sess, "structureProfiles", "DuplicateProfile", &[PyRep::Integer(prof_a)])
        .await.unwrap().as_i64().unwrap();
    assert_ne!(dup_prof, prof_a);

    rpc(&ctx, &mut dir_sess, "structureProfiles", "SetDefaultProfile", &[PyRep::Integer(prof_a)])
        .await
        .expect("SetDefaultProfile");
    let all_profs = rpc(&ctx, &mut dir_sess, "structureProfiles", "GetProfiles", &[]).await.unwrap();
    let PyRep::List(prof_entries) = all_profs else { panic!("expected list") };
    let prof_a_kv = prof_entries.iter().find(|p| kv_entry(p, "profileID").and_then(|v| v.as_i64()) == Some(prof_a)).unwrap();
    assert_eq!(kv_entry(prof_a_kv, "name"), Some(&PyRep::String("Industrial Hub V2".into())));
    assert_eq!(kv_entry(prof_a_kv, "isDefault"), Some(&PyRep::Bool(true)));

    // 4. Anchor a new Fortizar WITHOUT explicit profileID -> inherits corp default profile `prof_a`!
    let anchor_ship = dir_sess.ship_id.unwrap();
    rpc(&ctx, &mut dir_sess, "stationSvc", "Undock", &[]).await.unwrap();
    let st_item = 880_100i64;
    let anchor_sys = 30_000_144u32; // Perimeter (non-trade-hub highsec)
    let far_pos = DVec3::new(0.0, 5.0e9, 0.0);
    dir_sess.solar_system_id = Some(anchor_sys);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&dir_char) {
            ch.solar_system_id = anchor_sys;
        }
        if let Some(sh) = snap.items.get_mut(&anchor_ship) {
            sh.location_id = i64::from(anchor_sys);
        }
        snap.items.insert(st_item, InventoryItemRecord {
            item_id: st_item, type_id: 35833, owner_id: dir_char, location_id: anchor_ship,
            flag: InventoryFlag::Cargo, quantity: 1, singleton: false, version: 1, ..Default::default()
        });
        snap.item_positions.insert(anchor_ship, far_pos.to_array());
    });
    let sys_h = ctx.systems.get_or_spawn(anchor_sys);
    let _ = sys_h.spawn_ball(Ball {
        id: anchor_ship, type_id: 606, position: far_pos, velocity: DVec3::ZERO,
        max_velocity: 300.0, inertia: 2.0, mass: 1_000_000.0, radius: 40.0,
        speed_fraction: 0.0, mode: MovementMode::Stop,
    }).await;
    let _ = sys_h.set_ball_position(anchor_ship, far_pos).await;
    let anchor_rep = rpc(&ctx, &mut dir_sess, "structureDeployment", "Anchor", &[PyRep::Integer(st_item)])
        .await
        .expect("Anchor Fortizar inheriting default profile");
    let anchored_id = kv_entry(&anchor_rep, "structureID").and_then(|v| v.as_i64()).unwrap();

    // Bring structure operational with online Standup Reprocessing Facility I (35899) and Cloning Center I (35894)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(st) = snap.structures.get_mut(&anchored_id) {
            st.state = life::STATE_SHIELD_VULNERABLE.into();
            st.quantum_core_installed = true;
            st.fuel_blocks = 2_000;
        }
        snap.items.insert(880_101, InventoryItemRecord {
            item_id: 880_101, type_id: 35899, owner_id: corp_id, location_id: anchored_id,
            flag: InventoryFlag::ServiceSlot0, quantity: 1, singleton: true, version: 1, ..Default::default()
        });
        snap.items.insert(880_102, InventoryItemRecord {
            item_id: 880_102, type_id: 35894, owner_id: corp_id, location_id: anchored_id,
            flag: InventoryFlag::ServiceSlot1, quantity: 1, singleton: true, version: 1, ..Default::default()
        });
        snap.module_online_states.insert(880_101, true);
        snap.module_online_states.insert(880_102, true);
    });

    // DeleteProfile on in-use `prof_a` fails with ProfileInUse; deleting unused `dup_prof` succeeds!
    assert_eq!(
        rpc(&ctx, &mut dir_sess, "structureProfiles", "DeleteProfile", &[PyRep::Integer(prof_a)]).await.unwrap_err().code,
        "ProfileInUse"
    );
    rpc(&ctx, &mut dir_sess, "structureProfiles", "DeleteProfile", &[PyRep::Integer(dup_prof)])
        .await
        .expect("Delete unused duplicate profile");

    // 5. Verify per-service ACLs via structureSettings.CharacterCheckService / CharacterGetServices
    let mut dock_only_sess = SessionState::new(503);
    dock_only_sess.user_id = Some(2);
    dock_only_sess.character_id = Some(guest_dock_only);
    dock_only_sess.corporation_id = Some(ctx.db.with_snapshot(|s| s.characters[&guest_dock_only].corporation_id));
    assert_eq!(
        rpc(&ctx, &mut dock_only_sess, "structureSettings", "CharacterCheckService", &[PyRep::Integer(anchored_id), PyRep::Integer(4)]).await.unwrap_err().code,
        "StructureServiceAccessDenied"
    );

    let mut reproc_sess = SessionState::new(504);
    reproc_sess.user_id = Some(4);
    reproc_sess.character_id = Some(guest_reproc);
    reproc_sess.corporation_id = Some(98_000_040);
    rpc(&ctx, &mut reproc_sess, "structureSettings", "CharacterCheckService", &[PyRep::Integer(anchored_id), PyRep::Integer(4)])
        .await
        .expect("VIP pilot on reproc_acl must pass CharacterCheckService");
    let svcs = rpc(&ctx, &mut reproc_sess, "structureSettings", "CharacterGetServices", &[PyRep::Integer(anchored_id)])
        .await
        .unwrap();
    let PyRep::Dict(svc_entries) = svcs else { panic!("expected dict") };
    let reproc_rate = svc_entries.iter().find(|(k, _)| k.as_i64() == Some(4)).and_then(|(_, v)| v.as_f64()).unwrap();
    assert!((reproc_rate - 0.035).abs() < 1e-6);

    // 6. ChangeProfiles enforces a 300s (5-minute) security delay when switching an already-assigned profile!
    let corp_only_acl = group_id_of(&rpc(&ctx, &mut dir_sess, "ownerGroupManager", "Create", &[PyRep::String("Corp Only ACL".into()), PyRep::String("".into())]).await.unwrap());
    rpc(&ctx, &mut dir_sess, "ownerGroupManager", "UpdateMembershipTypes", &[PyRep::Integer(corp_only_acl), PyRep::List(vec![PyRep::Integer(corp_id)]), PyRep::Integer(0)])
        .await.unwrap();
    let lockdown_prof = rpc(&ctx, &mut dir_sess, "structureProfiles", "CreateProfile", &[PyRep::String("Corp Lockdown".into()), PyRep::String("Corp Only".into())])
        .await.unwrap().as_i64().unwrap();
    rpc(&ctx, &mut dir_sess, "structureProfiles", "SaveProfileSettings", &[
        PyRep::Integer(lockdown_prof),
        PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Real(0.0), PyRep::Integer(corp_only_acl)])]),
    ])
        .await.unwrap();
    rpc(&ctx, &mut dir_sess, "structureProfiles", "ChangeProfiles", &[PyRep::List(vec![PyRep::Integer(anchored_id)]), PyRep::Integer(lockdown_prof)])
        .await.unwrap();

    // At t + 120s (< 300s): old profile `prof_a` is STILL active (guest_reproc still has access)!
    ctx.advance_secs(120);
    assert_eq!(
        rpc(&ctx, &mut reproc_sess, "structureDirectory", "CheckMyDockingAccessToStructures", &[PyRep::List(vec![PyRep::Integer(anchored_id)])]).await.unwrap(),
        PyRep::List(vec![PyRep::Integer(anchored_id)])
    );

    // At t + 305s (> 300s): lockdown_prof takes effect -> guest_reproc loses docking & service access!
    ctx.advance_secs(185);
    assert_eq!(
        rpc(&ctx, &mut reproc_sess, "structureDirectory", "CheckMyDockingAccessToStructures", &[PyRep::List(vec![PyRep::Integer(anchored_id)])]).await.unwrap(),
        PyRep::List(vec![])
    );
    assert_eq!(
        rpc(&ctx, &mut reproc_sess, "structureSettings", "CharacterCheckService", &[PyRep::Integer(anchored_id), PyRep::Integer(4)]).await.unwrap_err().code,
        "StructureServiceAccessDenied"
    );
}

#[tokio::test]
async fn test_structure_directory_visibility_and_sensitive_masking() {
    // parity: feat:structures.structure-directory
    eve_testkit::attest(&["feat:structures.structure-directory"]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);

    let dir_char = 90_000_001;
    let acl_guest = 90_000_002;
    let stranger = 90_000_006;

    let mut dir_sess = login_with_corp(&ctx, 511, dir_char, "DIRX").await;
    let corp_id = ctx.db.with_snapshot(|s| s.characters[&dir_char].corporation_id);
    ctx.db.with_mut_snapshot(|snap| {
        let mut cs = snap.characters[&acl_guest].clone();
        cs.character_id = stranger;
        cs.corporation_id = 98_000_066;
        cs.name = "Stranger Pilot".into();
        snap.characters.insert(stranger, cs);
    });

    let lowsec_sys = 30_002_718u32; // Rancer (low-sec, valid for Cyno Beacon too)
    let highsec_sys = 30_000_142u32; // Jita (high-sec, required for War HQ)
    let fort_id = 1_000_000_200_001i64;
    let beacon_id = 1_000_000_200_002i64;
    let hq_fort_id = 1_000_000_200_003i64;
    seed_operational_structure(&ctx, fort_id, 35833, corp_id, lowsec_sys, [10_000_000.0, 0.0, 0.0]);
    seed_operational_structure(&ctx, beacon_id, 35840, corp_id, lowsec_sys, [20_000_000.0, 0.0, 0.0]);
    seed_operational_structure(&ctx, hq_fort_id, 35833, corp_id, highsec_sys, [30_000_000.0, 0.0, 0.0]);

    let acl_id = group_id_of(&rpc(&ctx, &mut dir_sess, "ownerGroupManager", "Create", &[PyRep::String("Directory ACL".into()), PyRep::String("".into())]).await.unwrap());
    rpc(&ctx, &mut dir_sess, "ownerGroupManager", "UpdateMembershipTypes", &[PyRep::Integer(acl_id), PyRep::List(vec![PyRep::Integer(acl_guest)]), PyRep::Integer(0)])
        .await.unwrap();
    let prof_id = rpc(&ctx, &mut dir_sess, "structureProfiles", "CreateProfile", &[PyRep::String("Shared Access".into()), PyRep::String("".into())])
        .await.unwrap().as_i64().unwrap();
    rpc(&ctx, &mut dir_sess, "structureProfiles", "SaveProfileSettings", &[
        PyRep::Integer(prof_id),
        PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Real(0.0), PyRep::Integer(acl_id)])]),
    ])
        .await.unwrap();
    rpc(&ctx, &mut dir_sess, "structureProfiles", "ChangeProfiles", &[PyRep::List(vec![PyRep::Integer(fort_id), PyRep::Integer(beacon_id)]), PyRep::Integer(prof_id)])
        .await.unwrap();

    // 1. Director sees full unmasked fields in GetMyCorporationStructures, GetStructureInfo, and GetValidWarHQs
    let corp_structs = rpc(&ctx, &mut dir_sess, "structureDirectory", "GetMyCorporationStructures", &[]).await.unwrap();
    let PyRep::Dict(corp_entries) = corp_structs else { panic!("expected dict") };
    let fort_dir = corp_entries.iter().find(|(k, _)| k.as_i64() == Some(fort_id)).map(|(_, v)| v).expect("fortizar in corp structures");
    assert_eq!(kv_entry(fort_dir, "fuelBlocks"), Some(&PyRep::Integer(2_400)));
    assert_eq!(kv_entry(fort_dir, "profileID"), Some(&PyRep::Integer(prof_id)));
    assert_eq!(kv_entry(fort_dir, "reinforceHour"), Some(&PyRep::Integer(18)));

    let war_hqs = rpc(&ctx, &mut dir_sess, "structureDirectory", "GetValidWarHQs", &[]).await.unwrap();
    let PyRep::List(hq_list) = war_hqs else { panic!("expected list") };
    assert!(hq_list.contains(&PyRep::Integer(hq_fort_id)), "Operational high-sec Fortizar must be a valid War HQ");
    assert!(!hq_list.contains(&PyRep::Integer(fort_id)), "Low-sec Fortizar cannot be a War HQ");
    assert!(!hq_list.contains(&PyRep::Integer(beacon_id)), "Flex Cyno Beacon cannot be a War HQ");

    // 2. ACL-authorized outsider sees Fortizar in GetMyDockableStructures & Cyno Beacon in GetMyAccessibleOnlineCynoBeaconStructures,
    //    AND sensitive management fields (`fuelBlocks`, `profileID`, `reinforceHour`, `stateTimerEnd`) are masked (`None`)!
    let mut guest_sess = SessionState::new(512);
    guest_sess.user_id = Some(2);
    guest_sess.character_id = Some(acl_guest);
    guest_sess.corporation_id = Some(ctx.db.with_snapshot(|s| s.characters[&acl_guest].corporation_id));
    guest_sess.solar_system_id = Some(lowsec_sys);

    let dockable = rpc(&ctx, &mut guest_sess, "structureDirectory", "GetMyDockableStructures", &[PyRep::Integer(lowsec_sys as i64)]).await.unwrap();
    let PyRep::List(dock_list) = dockable else { panic!("expected list") };
    assert!(dock_list.contains(&PyRep::Integer(fort_id)));

    let guest_info = rpc(&ctx, &mut guest_sess, "structureDirectory", "GetStructureInfo", &[PyRep::Integer(fort_id)]).await.unwrap();
    assert_eq!(kv_entry(&guest_info, "fuelBlocks"), Some(&PyRep::None));
    assert_eq!(kv_entry(&guest_info, "profileID"), Some(&PyRep::None));
    assert_eq!(kv_entry(&guest_info, "reinforceHour"), Some(&PyRep::None));

    let beacons = rpc(&ctx, &mut guest_sess, "structureDirectory", "GetMyAccessibleOnlineCynoBeaconStructures", &[]).await.unwrap();
    let PyRep::Dict(beacon_map) = beacons else { panic!("expected dict") };
    assert!(beacon_map.iter().any(|(k, _)| k.as_i64() == Some(beacon_id)));
    let beacon_sys = rpc(&ctx, &mut guest_sess, "structureDirectory", "GetSolarSystemsWithBeacons", &[]).await.unwrap();
    let PyRep::List(bs_list) = beacon_sys else { panic!("expected list") };
    assert!(bs_list.contains(&PyRep::Integer(lowsec_sys as i64)));

    // 3. Unauthorized stranger cannot see private Fortizar or Beacon in directory queries!
    let mut stranger_sess = SessionState::new(513);
    stranger_sess.user_id = Some(6);
    stranger_sess.character_id = Some(stranger);
    stranger_sess.corporation_id = Some(98_000_066);
    stranger_sess.solar_system_id = Some(lowsec_sys);

    let str_dock = rpc(&ctx, &mut stranger_sess, "structureDirectory", "GetMyDockableStructures", &[PyRep::Integer(lowsec_sys as i64)]).await.unwrap();
    let PyRep::List(str_list) = str_dock else { panic!("expected list") };
    assert!(!str_list.contains(&PyRep::Integer(fort_id)));
    assert_eq!(
        rpc(&ctx, &mut stranger_sess, "structureDirectory", "GetStructureInfo", &[PyRep::Integer(fort_id)]).await.unwrap_err().code,
        "StructureAccessDenied"
    );
    let str_beacons = rpc(&ctx, &mut stranger_sess, "structureDirectory", "GetMyAccessibleOnlineCynoBeaconStructures", &[]).await.unwrap();
    assert_eq!(str_beacons, PyRep::Dict(vec![]));
}

#[tokio::test]
async fn test_structure_auto_defense_retaliation_and_highsec_concord_rules() {
    // parity: feat:structures.defense-settings
    eve_testkit::attest(&["feat:structures.defense-settings"]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);

    let dir_char = 90_000_001;
    let friendly_char = 90_000_011;
    let attacker_char = 90_000_012;
    let neg_standing_char = 90_000_013;
    let blocked_char = 90_000_014;
    let war_char = 90_000_015;

    let mut dir_sess = login_with_corp(&ctx, 521, dir_char, "DEFN").await;
    let corp_id = ctx.db.with_snapshot(|s| s.characters[&dir_char].corporation_id);
    let enemy_war_corp = 98_000_515i64;
    let lowsec_sys = 30_002_718u32; // Rancer (sec < 0.45)
    let highsec_sys = 30_000_142u32; // Jita (sec >= 0.45)

    ctx.db.with_mut_snapshot(|snap| {
        let base = snap.characters[&90_000_002].clone();
        for (cid, ccorp, name) in [
            (friendly_char, corp_id, "Corp Mate"),
            (attacker_char, 98_000_512, "Neutral Aggressor"),
            (neg_standing_char, 98_000_513, "Red Pilot"),
            (blocked_char, 98_000_514, "Blocked Pilot"),
            (war_char, enemy_war_corp, "War Target"),
        ] {
            let mut ch = base.clone();
            ch.character_id = cid;
            ch.corporation_id = ccorp;
            ch.name = name.into();
            snap.characters.insert(cid, ch);
        }
        snap.corporations.insert(enemy_war_corp, eve_db::CorporationRecord {
            corporation_id: enemy_war_corp,
            name: "Hostile War Corp".into(),
            ticker: "WARZ".into(),
            ceo_id: war_char,
            members: std::collections::HashSet::from([war_char]),
            ..Default::default()
        });
        snap.wars.insert(9001, eve_db::WarRecord {
            war_id: 9001,
            aggressor_corp_id: enemy_war_corp,
            defender_corp_id: corp_id,
            started_unix: 1_000,
            mutual: true,
            active: true,
        });
    });

    // Set negative corporate contact (-5.0) against `neg_standing_char` via corpRegistry.AddCorporateContact
    rpc(&ctx, &mut dir_sess, "corpRegistry", "AddCorporateContact", &[PyRep::Integer(neg_standing_char), PyRep::Real(-5.0)])
        .await
        .expect("AddCorporateContact -5.0");

    // Create ACL with `corp_id` allowed (0) and `blocked_char` blocked (-1)
    let blk_acl = group_id_of(&rpc(&ctx, &mut dir_sess, "ownerGroupManager", "Create", &[PyRep::String("Defense Blocklist".into()), PyRep::String("".into())]).await.unwrap());
    rpc(&ctx, &mut dir_sess, "ownerGroupManager", "UpdateMembershipTypes", &[PyRep::Integer(blk_acl), PyRep::List(vec![PyRep::Integer(corp_id)]), PyRep::Integer(0)])
        .await.unwrap();
    rpc(&ctx, &mut dir_sess, "ownerGroupManager", "UpdateMembershipTypes", &[PyRep::Integer(blk_acl), PyRep::List(vec![PyRep::Integer(blocked_char)]), PyRep::Integer(-1)])
        .await.unwrap();

    // Create defense profile via structureProfiles.SaveProfileSettings using V24 3-tuple settings
    let def_prof = rpc(&ctx, &mut dir_sess, "structureProfiles", "CreateProfile", &[PyRep::String("Active Defense".into()), PyRep::String("".into())])
        .await.unwrap().as_i64().unwrap();
    rpc(
        &ctx,
        &mut dir_sess,
        "structureProfiles",
        "SaveProfileSettings",
        &[
            PyRep::Integer(def_prof),
            PyRep::List(vec![
                PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Real(0.0), PyRep::Integer(blk_acl)]),
                PyRep::Tuple(vec![
                    PyRep::Integer(17),
                    PyRep::Tuple(vec![
                        PyRep::Bool(true),
                        PyRep::Bool(true),
                        PyRep::Bool(true),
                        PyRep::Real(0.0),
                        PyRep::Bool(true),
                    ]),
                    PyRep::Integer(0),
                ]),
            ]),
        ],
    )
    .await
    .expect("SaveProfileSettings defense rules");

    let low_st = 1_000_000_300_001i64;
    let high_st = 1_000_000_300_002i64;
    seed_operational_structure(&ctx, low_st, 35833, corp_id, lowsec_sys, [0.0, 0.0, 0.0]);
    seed_operational_structure(&ctx, high_st, 35833, corp_id, highsec_sys, [0.0, 0.0, 0.0]);

    // Fit online Standup Multirole Missile Launcher I (35922) + Standup Mjolnir Cruise Missile (37847) to both Fortizars
    ctx.db.with_mut_snapshot(|snap| {
        for (mod_id, charge_id, sid) in [(910_001, 910_011, low_st), (910_002, 910_012, high_st)] {
            snap.items.insert(mod_id, InventoryItemRecord {
                item_id: mod_id, type_id: 35922, owner_id: corp_id, location_id: sid,
                flag: InventoryFlag::HiSlot0, quantity: 1, singleton: true, version: 1, ..Default::default()
            });
            snap.items.insert(charge_id, InventoryItemRecord {
                item_id: charge_id, type_id: 37847, owner_id: corp_id, location_id: sid,
                flag: InventoryFlag::HiSlot0, quantity: 100, singleton: false, version: 1, ..Default::default()
            });
            snap.module_online_states.insert(mod_id, true);
        }
    });
    rpc(&ctx, &mut dir_sess, "structureProfiles", "ChangeProfiles", &[PyRep::List(vec![PyRep::Integer(low_st), PyRep::Integer(high_st)]), PyRep::Integer(def_prof)])
        .await.unwrap();

    // Helper to spawn a pilot on grid and trigger grid update via `beyonce.UpdateStateRequest`
    async fn spawn_and_enter_grid(ctx: &ServerContext, sess_id: u64, cid: i64, ship_id: i64, sys_id: u32, pos: DVec3) -> SessionState {
        ctx.db.with_mut_snapshot(|snap| {
            let ch = snap.characters.get_mut(&cid).unwrap();
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = ship_id;
            snap.items.insert(ship_id, InventoryItemRecord {
                item_id: ship_id, type_id: 626, owner_id: cid, location_id: sys_id as i64,
                flag: InventoryFlag::None, quantity: 1, singleton: true, version: 1, ..Default::default()
            });
            snap.item_positions.insert(ship_id, pos.to_array());
        });
        let sys_h = ctx.systems.get_or_spawn(sys_id);
        let _ = sys_h
            .spawn_ball_with_meta(
                Ball {
                    id: ship_id, type_id: 626, position: pos, velocity: DVec3::ZERO, max_velocity: 250.0,
                    inertia: 4.0, mass: 11_000_000.0, radius: 130.0, speed_fraction: 0.0, mode: MovementMode::Stop,
                },
                ctx.build_ship_combat_state(cid, ship_id, 626),
                ctx.build_ship_slim_meta(cid, 626),
            )
            .await;
        let user_id = ctx.db.with_snapshot(|sn| sn.characters[&cid].user_id);
        let _ = ctx.db.acquire_online_lock_with_computer_hash(user_id, sess_id, Some(cid), None, true, 1_800_000_000);
        let mut s = SessionState::new(sess_id);
        s.user_id = Some(user_id);
        s.character_id = Some(cid);
        s.corporation_id = Some(ctx.db.with_snapshot(|sn| sn.characters[&cid].corporation_id));
        s.solar_system_id = Some(sys_id);
        s.ship_id = Some(ship_id);
        let _ = rpc(ctx, &mut s, "beyonce", "UpdateStateRequest", &[]).await;
        s
    }

    let low_sys_h = ctx.systems.get_or_spawn(lowsec_sys);

    // 1. Friendly corp member entering low-sec grid is NEVER targeted or damaged
    let _ = spawn_and_enter_grid(&ctx, 522, friendly_char, 920_001, lowsec_sys, DVec3::new(15_000.0, 0.0, 0.0)).await;
    let f_cs = low_sys_h.get_combat_state(920_001).await.unwrap().unwrap();
    assert!((f_cs.shield_hp - f_cs.max_shield_hp).abs() < 1e-6, "Friendly ship must take zero auto-defense damage");

    // 2. War target, negative-standing pilot, and blocked-ACL pilot entering low-sec grid ARE auto-locked and damaged!
    for (idx, (cid, ship_id)) in [(war_char, 920_002), (neg_standing_char, 920_003), (blocked_char, 920_004)].into_iter().enumerate() {
        ctx.advance_secs(15);
        let _s = spawn_and_enter_grid(&ctx, 530 + idx as u64, cid, ship_id, lowsec_sys, DVec3::new(18_000.0, idx as f64 * 2_000.0, 0.0)).await;
        let cs = low_sys_h.get_combat_state(ship_id).await.unwrap().unwrap();
        assert!(cs.shield_hp < cs.max_shield_hp, "Hostile/red/blocked pilot {cid} in low-sec must take auto-defense volley damage");
        let notifs = ctx.drain_notifications_for_char(cid);
        assert!(notifs.iter().any(|n| n.event == "OnTarget"), "Target must receive OnTarget lock notification");
        assert!(notifs.iter().any(|n| n.event == "OnDamageMessage"), "Target must receive OnDamageMessage");
    }

    // 3. Neutral pilot enters grid unharmed until firing on the structure -> live combat retaliation!
    for sid in [920_002, 920_003, 920_004] { let _ = low_sys_h.remove_ball(sid).await; }
    ctx.advance_secs(15);
    let mut att_sess = spawn_and_enter_grid(&ctx, 540, attacker_char, 920_005, lowsec_sys, DVec3::new(12_000.0, 0.0, 0.0)).await;
    let pre_cs = low_sys_h.get_combat_state(920_005).await.unwrap().unwrap();
    assert!((pre_cs.shield_hp - pre_cs.max_shield_hp).abs() < 1e-6, "Neutral pilot is unharmed before attacking");

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(920_105, InventoryItemRecord {
            item_id: 920_105, type_id: 47272, owner_id: attacker_char, location_id: 920_005,
            flag: InventoryFlag::HiSlot0, quantity: 1, singleton: true, version: 1, ..Default::default()
        });
        snap.items.insert(920_106, InventoryItemRecord {
            item_id: 920_106, type_id: 47925, owner_id: attacker_char, location_id: 920_005,
            flag: InventoryFlag::HiSlot0, quantity: 50, singleton: false, version: 1, ..Default::default()
        });
    });
    ctx.invalidate_ship_fitting(920_005);
    eve_server::services::station_structure::structure_lifecycle_rpc::ensure_structure_ball_in_space(&ctx, lowsec_sys, low_st).await;
    low_sys_h.add_target_lock(920_005, TargetLock { target_id: low_st, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(50_000.0) }).await.unwrap();
    ctx.advance_secs(15);
    rpc(&ctx, &mut att_sess, "dogmaIM", "Activate", &[PyRep::Integer(920_105), PyRep::Integer(low_st)])
        .await
        .expect("Neutral fires weapon at Fortizar");
    let post_cs = low_sys_h.get_combat_state(920_005).await.unwrap().unwrap();
    assert!(post_cs.shield_hp < post_cs.max_shield_hp, "Structure must immediately retaliate against live weapon attacker");

    // 4. High-sec CONCORD safety: negative-standing and blocked-ACL pilots in Jita are NOT illegally auto-attacked, whereas war target IS!
    let high_sys_h = ctx.systems.get_or_spawn(highsec_sys);
    let _ = spawn_and_enter_grid(&ctx, 550, neg_standing_char, 920_013, highsec_sys, DVec3::new(15_000.0, 0.0, 0.0)).await;
    let hs_neg_cs = high_sys_h.get_combat_state(920_013).await.unwrap().unwrap();
    assert!((hs_neg_cs.shield_hp - hs_neg_cs.max_shield_hp).abs() < 1e-6, "High-sec structure must NOT illegally fire on non-wartarget negative standing pilot");

    let _ = spawn_and_enter_grid(&ctx, 551, war_char, 920_015, highsec_sys, DVec3::new(16_000.0, 0.0, 0.0)).await;
    let hs_war_cs = high_sys_h.get_combat_state(920_015).await.unwrap().unwrap();
    assert!(hs_war_cs.shield_hp < hs_war_cs.max_shield_hp, "High-sec structure MUST still auto-engage legal mutual war target");

    // 5. Disabling `autoDefenseEnabled` stops auto-defense on newly arriving war targets
    rpc(
        &ctx,
        &mut dir_sess,
        "structureProfiles",
        "SaveProfileSettings",
        &[
            PyRep::Integer(def_prof),
            PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Integer(17),
                PyRep::Bool(false),
                PyRep::Integer(0),
            ])]),
        ],
    )
        .await.unwrap();
    let _ = spawn_and_enter_grid(&ctx, 552, war_char, 920_099, lowsec_sys, DVec3::new(19_000.0, 0.0, 0.0)).await;
    let disabled_cs = low_sys_h.get_combat_state(920_099).await.unwrap().unwrap();
    assert!((disabled_cs.shield_hp - disabled_cs.max_shield_hp).abs() < 1e-6, "Disabled autoDefenseEnabled must not fire");
}

#[allow(dead_code, unused_imports)]
pub(crate) mod mobile_structures_core {
    use super::*;

//  Integration tests for Mobile Deployable Structures (`feat:structures.mobile-depot`,
//  `feat:structures.mobile-depot-lifetime-reinforce`, `feat:structures.mobile-deploy-restrictions`).


use std::collections::HashMap;
use glam::DVec3;
use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, DamageVector, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::mobile_deployables::*;
use eve_server::services::inventory_fitting::fitting_containers_ships::{
    is_near_in_space_fitting_service, validate_single_item_can_be_opened,
};

const PILOT_1: i64 = 90_000_001;
const PILOT_2: i64 = 90_000_002;
const SHIP_1: i64 = 100_000_001;
const SHIP_2: i64 = 100_000_002;
const TEST_SYSTEM: u32 = 30_000_142; // Jita (High-sec 0.95)

async fn setup_pilot(
    ctx: &ServerContext,
    sess_id: u64,
    char_id: i64,
    ship_id: i64,
    sys_id: u32,
    pos: DVec3,
) -> SessionState {
    let mut sess = SessionState::new(sess_id);
    sess.user_id = Some(char_id as u64);
    sess.character_id = Some(char_id);
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = ship_id;
        }
        snap.items.insert(ship_id, InventoryItemRecord {
            item_id: ship_id, type_id: 587, owner_id: char_id, location_id: sys_id as i64,
            flag: InventoryFlag::None, quantity: 1, singleton: true, ..Default::default()
        });
        snap.item_positions.insert(ship_id, pos.to_array());
    });

    let sys = ctx.systems.get_or_spawn(sys_id);
    let ball = Ball {
        id: ship_id, type_id: 587, position: pos, velocity: DVec3::ZERO, max_velocity: 300.0,
        inertia: 1.0, mass: 1_000_000.0, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop,
    };
    let _ = sys.spawn_ball_with_meta(
        ball,
        ctx.build_ship_combat_state(char_id, ship_id, 587),
        ctx.build_ship_slim_meta(char_id, 587),
    ).await;

    sess
}

/// Tests Mobile Depot anchoring countdown, refitting proximity & owner gating, storage access,
/// 48-hour reinforcement threshold at <= 25% shield HP, invulnerability while reinforced,
/// and damage retention on scoop/redeploy (`feat:structures.mobile-depot`, `feat:structures.mobile-depot-lifetime-reinforce`).
#[tokio::test]
async fn test_mobile_depot_anchoring_refit_storage_reinforce() {
    // parity: feat:structures.mobile-depot, feat:structures.mobile-depot-lifetime-reinforce
    eve_testkit::attest(&[
        "feat:structures.mobile-depot",
        "feat:structures.mobile-depot-lifetime-reinforce",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let _start_unix = ctx.now_unix();
    let ship_pos = DVec3::new(10_000_000.0, 10_000_000.0, 10_000_000.0);
    let mut sess1 = setup_pilot(&ctx, 101, PILOT_1, SHIP_1, TEST_SYSTEM, ship_pos).await;

    let depot_item_id = 700_001i64;
    ctx.db.with_mut_snapshot(|snap| {
        let skills = snap.skills.entry(PILOT_1).or_default();
        skills.insert(11584, eve_db::TrainedSkillRecord { type_id: 11584, level: 3, skill_points: 40_000.0 });
        snap.items.insert(depot_item_id, InventoryItemRecord {
            item_id: depot_item_id, type_id: TYPE_MOBILE_DEPOT, owner_id: PILOT_1,
            location_id: SHIP_1, flag: InventoryFlag::Cargo, quantity: 1, singleton: true, ..Default::default()
        });
    });

    let kw = HashMap::new();
    let res = ctx.dispatch_rpc(
        &mut sess1,
        "posMgr",
        None,
        "AnchorStructure",
        &[PyRep::Integer(depot_item_id)],
        &kw,
        None,
    ).await;
    assert!(res.is_ok(), "Mobile Depot anchor failed: {res:?}");

    let rec = read_deployable_state(&ctx).deployables.get(&depot_item_id).cloned().expect("depot record");
    assert_eq!(rec.anchor_duration_secs, DEPOT_ANCHOR_TIME_SECS);
    assert!(!rec.anchor_completed);

    // 1. During anchoring phase: refit service MUST be unavailable
    let near_while_anchoring = is_near_in_space_fitting_service(&ctx, TEST_SYSTEM, SHIP_1).await;
    assert!(!near_while_anchoring, "Depot must not provide refit service while anchoring");

    // 2. During anchoring phase: takes direct damage and does NOT trigger reinforcement
    let sys = ctx.systems.get_or_spawn(TEST_SYSTEM);
    let anchor_dmg = DamageVector { em: 100.0, thermal: 0.0, kinetic: 0.0, explosive: 0.0 };
    let (dmg_done, destroyed, _) = sys.apply_damage(depot_item_id, anchor_dmg).await.expect("apply damage").expect("depot hit");
    assert!(dmg_done > 0.0, "Mobile Depot takes direct damage during 60s anchoring phase");
    assert!(!destroyed);
    let rec_anchor = read_deployable_state(&ctx).deployables.get(&depot_item_id).cloned().unwrap();
    assert!(rec_anchor.reinforced_until_unix.is_none(), "Depot must not reinforce during anchoring phase");

    // Advance clock past the 60s anchoring timer
    ctx.advance_secs(DEPOT_ANCHOR_TIME_SECS + 1);

    // 3. Anchoring complete: owner can refit within 2,500m
    let near_owner = is_near_in_space_fitting_service(&ctx, TEST_SYSTEM, SHIP_1).await;
    assert!(near_owner, "Pilot 1 (owner) within 2,500m of active Mobile Depot must have fitting service access");

    // Non-owner cannot use depot for refitting
    let mut _sess2 = setup_pilot(&ctx, 102, PILOT_2, SHIP_2, TEST_SYSTEM, ship_pos + DVec3::new(100.0, 0.0, 0.0)).await;
    let near_non_owner = is_near_in_space_fitting_service(&ctx, TEST_SYSTEM, SHIP_2).await;
    assert!(!near_non_owner, "Pilot 2 (non-owner) must NOT have fitting service access from Pilot 1's depot");

    // Owner beyond 2,500m cannot refit
    let _ = sys.set_ball_position(SHIP_1, ship_pos + DVec3::new(10_000.0, 0.0, 0.0)).await;
    let far_owner = is_near_in_space_fitting_service(&ctx, TEST_SYSTEM, SHIP_1).await;
    assert!(!far_owner, "Owner beyond 2,500m must NOT have fitting service access");
    let _ = sys.set_ball_position(SHIP_1, ship_pos).await;

    // 4. Storage access: only owner can open depot
    assert!(validate_single_item_can_be_opened(&ctx, PILOT_1, depot_item_id).await.is_ok());
    let non_owner_res = validate_single_item_can_be_opened(&ctx, PILOT_2, depot_item_id).await;
    assert!(non_owner_res.is_err());
    assert_eq!(non_owner_res.unwrap_err().code, "OwnershipViolation");

    // Test moving an item in and out of the depot via real invbroker.Add RPC
    let stored_item_id = 800_001i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(stored_item_id, InventoryItemRecord {
            item_id: stored_item_id, type_id: 34, owner_id: PILOT_1,
            location_id: SHIP_1, flag: InventoryFlag::Cargo, quantity: 50, singleton: false, ..Default::default()
        });
    });
    ctx.dispatch_rpc(
        &mut sess1,
        "invbroker",
        None,
        "Add",
        &[PyRep::Integer(stored_item_id), PyRep::Integer(depot_item_id), PyRep::Integer(InventoryFlag::Cargo as i64)],
        &kw,
        None,
    ).await.expect("invbroker.Add to depot");
    let has_item = ctx.db.with_snapshot(|snap| snap.items.get(&stored_item_id).is_some_and(|i| i.location_id == depot_item_id));
    assert!(has_item, "Item must be stored inside Mobile Depot");

    ctx.dispatch_rpc(
        &mut sess1,
        "invbroker",
        None,
        "Add",
        &[PyRep::Integer(stored_item_id), PyRep::Integer(SHIP_1), PyRep::Integer(InventoryFlag::Cargo as i64)],
        &kw,
        None,
    ).await.expect("invbroker.Add back to ship");
    let back_in_ship = ctx.db.with_snapshot(|snap| snap.items.get(&stored_item_id).is_some_and(|i| i.location_id == SHIP_1));
    assert!(back_in_ship, "Item must be moved back to ship cargo");

    // 5. Reinforcement threshold at <= 25% shield HP
    let depot_cs = sys.get_combat_state(depot_item_id).await.unwrap().expect("depot combat state");
    let shield_threshold = depot_cs.max_shield_hp * DEPOT_REINFORCE_SHIELD_THRESHOLD;
    let current_shield = depot_cs.shield_hp;
    assert!(current_shield > shield_threshold);

    // Apply damage to bring shields to <= 25% threshold
    let damage_to_trigger = DamageVector {
        em: current_shield - shield_threshold + 500.0,
        thermal: 0.0, kinetic: 0.0, explosive: 0.0,
    };
    let now_before_hit = ctx.now_unix();
    let (dmg_applied, destroyed_reinf, _) = sys.apply_damage(depot_item_id, damage_to_trigger).await.unwrap().expect("hit");
    assert!(!destroyed_reinf);
    assert_eq!(dmg_applied, current_shield - shield_threshold, "Damage applied must stop exactly at 25% shield threshold");

    let rec_reinf = read_deployable_state(&ctx).deployables.get(&depot_item_id).cloned().expect("rec");
    let expected_reinf_until = now_before_hit.saturating_add(DEPOT_REINFORCE_DURATION_SECS);
    assert_eq!(rec_reinf.reinforced_until_unix, Some(expected_reinf_until), "Reinforce duration must be exactly 48 hours (172,800s)");

    // 6. Invulnerability while reinforced
    let follow_up_damage = DamageVector { em: 500.0, thermal: 500.0, kinetic: 500.0, explosive: 500.0 };
    let (reinf_dmg_done, reinf_destroyed, _) = sys.apply_damage(depot_item_id, follow_up_damage).await.unwrap().expect("hit");
    assert_eq!(reinf_dmg_done, 0.0, "Reinforced depot must take 0 damage (invulnerable)");
    assert!(!reinf_destroyed);

    // 7. Damage retention across scoop and redeploy
    let depot_2_pos = ship_pos + DVec3::new(10_000.0, 0.0, 0.0);
    ctx.db.with_mut_snapshot(|snap| snap.item_positions.insert(SHIP_1, depot_2_pos.to_array()));
    let _ = sys.set_ball_position(SHIP_1, depot_2_pos).await;

    let depot_2_id = 700_002i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(depot_2_id, InventoryItemRecord {
            item_id: depot_2_id, type_id: TYPE_MOBILE_DEPOT, owner_id: PILOT_1,
            location_id: SHIP_1, flag: InventoryFlag::Cargo, quantity: 1, singleton: true, ..Default::default()
        });
    });
    ctx.dispatch_rpc(&mut sess1, "posMgr", None, "AnchorStructure", &[PyRep::Integer(depot_2_id)], &kw, None).await.unwrap();
    ctx.advance_secs(DEPOT_ANCHOR_TIME_SECS + 1);

    // Attempt re-anchor exploit (MD-P1-2): re-calling AnchorStructure on already anchored depot
    let reanchor_err = ctx.dispatch_rpc(&mut sess1, "posMgr", None, "AnchorStructure", &[PyRep::Integer(depot_2_id)], &kw, None).await.unwrap_err();
    assert_eq!(reanchor_err.code, "StructureAlreadyAnchored", "Re-anchoring already anchored structure must fail");

    // Deal 50% shield damage to depot 2
    let d2_cs = sys.get_combat_state(depot_2_id).await.unwrap().unwrap();
    let dmg_half = DamageVector { em: d2_cs.max_shield_hp * 0.50, thermal: 0.0, kinetic: 0.0, explosive: 0.0 };
    sys.apply_damage(depot_2_id, dmg_half).await.unwrap().unwrap();
    let d2_post_cs = sys.get_combat_state(depot_2_id).await.unwrap().unwrap();
    let expected_damaged_shields = d2_post_cs.shield_hp;

    // Unanchor depot 2 via real entity.CmdUnanchor RPC
    ctx.dispatch_rpc(&mut sess1, "entity", None, "CmdUnanchor", &[PyRep::Integer(depot_2_id)], &kw, None).await.expect("CmdUnanchor depot 2");

    // Non-owner scoop negative test (MD-P1-3)
    ctx.db.with_mut_snapshot(|snap| snap.item_positions.insert(SHIP_2, (depot_2_pos + DVec3::new(100.0, 0.0, 0.0)).to_array()));
    let _ = sys.set_ball_position(SHIP_2, depot_2_pos + DVec3::new(100.0, 0.0, 0.0)).await;
    let non_owner_scoop = ctx.dispatch_rpc(&mut _sess2, "ship", None, "Scoop", &[PyRep::Integer(depot_2_id)], &kw, None).await.unwrap_err();
    assert_eq!(non_owner_scoop.code, "OwnershipViolation", "Non-owner scoop must fail with OwnershipViolation");

    // Loaded depot scoop negative test (MD-R2): cannot scoop non-empty container
    let inside_item_id = 800_005i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(inside_item_id, InventoryItemRecord {
            item_id: inside_item_id, type_id: 34, owner_id: PILOT_1,
            location_id: depot_2_id, flag: InventoryFlag::Cargo, quantity: 10, singleton: false, ..Default::default()
        });
    });
    let loaded_scoop = ctx.dispatch_rpc(&mut sess1, "ship", None, "Scoop", &[PyRep::Integer(depot_2_id)], &kw, None).await.unwrap_err();
    assert_eq!(loaded_scoop.code, "CannotPutContainerInContainer", "Scooping loaded deployable must fail");
    assert_eq!(ctx.db.with_snapshot(|s| s.items.get(&inside_item_id).map(|i| i.location_id)), Some(depot_2_id));
    assert_eq!(ctx.db.with_snapshot(|s| s.items.get(&depot_2_id).map(|i| i.location_id)), Some(TEST_SYSTEM as i64));
    ctx.db.with_mut_snapshot(|snap| { snap.items.remove(&inside_item_id); });

    // Scoop depot 2 via real ship.Scoop RPC
    ctx.dispatch_rpc(&mut sess1, "ship", None, "Scoop", &[PyRep::Integer(depot_2_id)], &kw, None).await.expect("Scoop depot 2");
    assert_eq!(ctx.db.with_snapshot(|s| s.items.get(&depot_2_id).map(|i| i.location_id)), Some(SHIP_1));
    assert!(ctx.db.with_snapshot(|s| s.items.get(&depot_2_id).is_some_and(|i| i.singleton)), "Scooped deployable must retain singleton=true");

    // Redeploy depot 2
    let _redeploy_pos = ship_pos + DVec3::new(100.0, 0.0, 0.0);
    ctx.dispatch_rpc(&mut sess1, "posMgr", None, "AnchorStructure", &[PyRep::Integer(depot_2_id)], &kw, None).await.unwrap();
    let redeployed_cs = sys.get_combat_state(depot_2_id).await.unwrap().expect("redeployed combat state");
    assert!((redeployed_cs.shield_hp - expected_damaged_shields).abs() < 1.0, "Redeployed depot must retain pre-scoop damage state");

    // Redeploy below 25% threshold asserting destruction instead of reinforcement (MD-P1-3)
    ctx.advance_secs(DEPOT_ANCHOR_TIME_SECS + 1);
    let dmg_to_reinf = DamageVector { em: redeployed_cs.shield_hp + 500.0, thermal: 0.0, kinetic: 0.0, explosive: 0.0 };
    sys.apply_damage(depot_2_id, dmg_to_reinf).await.unwrap().unwrap();
    ctx.advance_secs(DEPOT_REINFORCE_DURATION_SECS + 1);
    sys.step_manual(1.0).await.unwrap();

    ctx.dispatch_rpc(&mut sess1, "entity", None, "CmdUnanchor", &[PyRep::Integer(depot_2_id)], &kw, None).await.expect("CmdUnanchor depot 2 post-reinf");
    ctx.dispatch_rpc(&mut sess1, "ship", None, "Scoop", &[PyRep::Integer(depot_2_id)], &kw, None).await.expect("Scoop depot 2 post-reinf");

    ctx.dispatch_rpc(&mut sess1, "posMgr", None, "AnchorStructure", &[PyRep::Integer(depot_2_id)], &kw, None).await.expect("redeploy post-reinf");
    ctx.advance_secs(DEPOT_ANCHOR_TIME_SECS + 1);
    sys.step_manual(1.0).await.unwrap();

    // Attack while shields at 0% (at/below reinforcement threshold) asserting destruction instead of reinforcement (MD-R3)
    let mut cs_depleted = sys.get_combat_state(depot_2_id).await.unwrap().unwrap();
    cs_depleted.shield_hp = 0.0;
    sys.set_combat_state(depot_2_id, cs_depleted).await.unwrap();

    let fatal_dmg = DamageVector { em: 50_000.0, thermal: 50_000.0, kinetic: 50_000.0, explosive: 50_000.0 };
    let (_, destroyed_after_redeploy, _) = sys.apply_damage(depot_2_id, fatal_dmg).await.unwrap().expect("hit");
    assert!(destroyed_after_redeploy, "Depot attacked while shields <= 25% must be destroyed without reinforcement");
}

/// Tests Mobile Depot 30-day lifetime enforcement, interaction refresh, and live expiry sweep (`feat:structures.mobile-depot-lifetime-reinforce`).
#[tokio::test]
async fn test_mobile_depot_lifetime_and_interaction_refresh() {
    // parity: feat:structures.mobile-depot-lifetime-reinforce
    eve_testkit::attest(&["feat:structures.mobile-depot-lifetime-reinforce"]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let start_unix = ctx.now_unix();
    let ship_pos = DVec3::new(20_000_000.0, 20_000_000.0, 20_000_000.0);
    let mut sess = setup_pilot(&ctx, 201, PILOT_1, SHIP_1, TEST_SYSTEM, ship_pos).await;

    let depot_id = 700_010i64;
    ctx.db.with_mut_snapshot(|snap| {
        let skills = snap.skills.entry(PILOT_1).or_default();
        skills.insert(11584, eve_db::TrainedSkillRecord { type_id: 11584, level: 3, skill_points: 40_000.0 });
        snap.items.insert(depot_id, InventoryItemRecord {
            item_id: depot_id, type_id: TYPE_MOBILE_DEPOT, owner_id: PILOT_1,
            location_id: SHIP_1, flag: InventoryFlag::Cargo, quantity: 1, singleton: true, ..Default::default()
        });
    });

    let kw = HashMap::new();
    ctx.dispatch_rpc(&mut sess, "posMgr", None, "AnchorStructure", &[PyRep::Integer(depot_id)], &kw, None).await.unwrap();

    let rec0 = read_deployable_state(&ctx).deployables.get(&depot_id).cloned().expect("depot record");
    assert_eq!(rec0.expires_at_unix, start_unix + DEPOT_LIFETIME_SECS);

    // Advance clock by 15 days
    ctx.advance_secs(15 * 86_400);
    let sys = ctx.systems.get_or_spawn(TEST_SYSTEM);
    sys.step_manual(1.0).await.unwrap();
    assert!(sys.get_combat_state(depot_id).await.unwrap().is_some(), "Depot must remain alive at day 15");

    // Pilot interacts with Mobile Depot via in-space refitting service -> resets the 30-day lifetime from current time
    let fitting_available = is_near_in_space_fitting_service(&ctx, TEST_SYSTEM, SHIP_1).await;
    assert!(fitting_available, "Pilot must have in-space fitting service near depot");
    let rec1 = read_deployable_state(&ctx).deployables.get(&depot_id).cloned().expect("depot record");
    let expected_refreshed_expiry = start_unix + 15 * 86_400 + DEPOT_LIFETIME_SECS;
    assert_eq!(rec1.expires_at_unix, expected_refreshed_expiry, "Interaction must refresh 30-day lifetime from current timestamp");

    // Advance clock another 20 days (total 35 days elapsed from deploy; would have expired without refresh)
    ctx.advance_secs(20 * 86_400);
    sys.step_manual(1.0).await.unwrap();
    assert!(sys.get_combat_state(depot_id).await.unwrap().is_some(), "Depot must remain alive at day 35 due to refreshed lifetime");

    // Advance clock another 15 days (total 50 days elapsed, exceeding the refreshed 45-day expiry)
    ctx.advance_secs(15 * 86_400 + 1);
    sys.step_manual(1.0).await.unwrap();

    // Verify live expiry sweep removed the depot ball from ballpark and cleaned up the item from DB
    assert!(sys.get_combat_state(depot_id).await.unwrap().is_none(), "Expired depot must be removed from ballpark");
    let item_in_db = ctx.db.with_snapshot(|snap| snap.items.contains_key(&depot_id));
    assert!(!item_in_db, "Expired depot must be removed from database");
    let rec_in_state = read_deployable_state(&ctx).deployables.contains_key(&depot_id);
    assert!(!rec_in_state, "Expired depot must be removed from deployable runtime state");
}

/// Tests deployment restrictions in starter/career systems, 50km proximity rules,
/// Mobile Warp Disruptor (group 361) nullsec gate regression, and Crimewatch suspect flag (`feat:structures.mobile-deploy-restrictions`).
#[tokio::test]
async fn test_mobile_deploy_restrictions_and_gate_bubble_regression() {
    // parity: feat:structures.mobile-deploy-restrictions
    eve_testkit::attest(&["feat:structures.mobile-deploy-restrictions"]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let ship_pos = DVec3::new(10_000_000.0, 10_000_000.0, 10_000_000.0);
    let _sess = setup_pilot(&ctx, 301, PILOT_1, SHIP_1, TEST_SYSTEM, ship_pos).await;

    // 1. Gating in starter / career agent systems
    let starter_sys = ctx.sde.schools.values().find(|s| (11..=22).contains(&s.school_id)).map(|s| s.solar_system_id).unwrap_or(30000140);
    let depot_item_id = 700_020i64;
    ctx.db.with_mut_snapshot(|snap| {
        let skills = snap.skills.entry(PILOT_1).or_default();
        skills.insert(11584, eve_db::TrainedSkillRecord { type_id: 11584, level: 3, skill_points: 40_000.0 });
        snap.items.insert(depot_item_id, InventoryItemRecord {
            item_id: depot_item_id, type_id: TYPE_MOBILE_DEPOT, owner_id: PILOT_1,
            location_id: SHIP_1, flag: InventoryFlag::Cargo, quantity: 1, singleton: true, ..Default::default()
        });
    });

    let mut starter_sess = setup_pilot(&ctx, 302, PILOT_1, SHIP_1, starter_sys, ship_pos).await;
    let kw = HashMap::new();
    let starter_res = ctx.dispatch_rpc(
        &mut starter_sess,
        "posMgr",
        None,
        "AnchorStructure",
        &[PyRep::Integer(depot_item_id)],
        &kw,
        None,
    ).await;
    assert!(starter_res.is_err());
    assert_eq!(starter_res.unwrap_err().code, "RestrictionsInEffect", "Must block mobile structures in starter systems");

    // 2. Proximity to Stargate (>= 50km)
    let gate_pos = ctx.sde.stargates().values().find(|g| g.from_solar_system_id == TEST_SYSTEM).map(|g| g.position).unwrap_or(DVec3::ZERO);
    let mut gate_sess = setup_pilot(&ctx, 303, PILOT_1, SHIP_1, TEST_SYSTEM, gate_pos + DVec3::new(10_000.0, 0.0, 0.0)).await;
    let gate_res = ctx.dispatch_rpc(
        &mut gate_sess,
        "posMgr",
        None,
        "AnchorStructure",
        &[PyRep::Integer(depot_item_id)],
        &kw,
        None,
    ).await;
    assert!(gate_res.is_err());
    assert_eq!(gate_res.unwrap_err().code, "DeploymentTooCloseToGate", "Must block mobile deployable within 50km of stargates");

    // 3. Regression test (MD-03): Mobile Warp Disruptor (group 361) at Stargate in nullsec
    let (nullsec_sys, nullsec_gate_pos) = ctx.sde.stargates().values()
        .filter_map(|g| {
            let sys = ctx.sde.solar_systems.get(&g.from_solar_system_id)?;
            if sys.security_status <= 0.0 && sys.solar_system_id < 31_000_000 {
                Some((sys.solar_system_id, g.position))
            } else {
                None
            }
        })
        .next()
        .expect("nullsec system with stargate");
    let mwd_item_id = 700_021i64;
    let mwd_type_id = 12198; // Mobile Small Warp Disruptor I (group 361)
    ctx.db.with_mut_snapshot(|snap| {
        let skills = snap.skills.entry(PILOT_1).or_default();
        skills.insert(3435, eve_db::TrainedSkillRecord { type_id: 3435, level: 5, skill_points: 256_000.0 });
        skills.insert(11584, eve_db::TrainedSkillRecord { type_id: 11584, level: 5, skill_points: 256_000.0 });
        snap.items.insert(mwd_item_id, InventoryItemRecord {
            item_id: mwd_item_id, type_id: mwd_type_id, owner_id: PILOT_1,
            location_id: SHIP_1, flag: InventoryFlag::Cargo, quantity: 1, singleton: true, ..Default::default()
        });
    });
    let mut nullsec_sess = setup_pilot(&ctx, 304, PILOT_1, SHIP_1, nullsec_sys, nullsec_gate_pos + DVec3::new(10_000.0, 0.0, 0.0)).await;
    let mwd_res = ctx.dispatch_rpc(
        &mut nullsec_sess,
        "posMgr",
        None,
        "AnchorStructure",
        &[PyRep::Integer(mwd_item_id)],
        &kw,
        None,
    ).await;
    assert!(mwd_res.is_ok(), "Nullsec Mobile Warp Disruptor MUST be allowed to anchor within 50km of stargate: {mwd_res:?}");

    // 4. Proximity between two Mobile Depots (>= 5km)
    let valid_pos = DVec3::new(30_000_000.0, 30_000_000.0, 30_000_000.0);
    let mut sess_valid = setup_pilot(&ctx, 305, PILOT_1, SHIP_1, TEST_SYSTEM, valid_pos).await;
    let depot_1_id = 700_022i64;
    let depot_2_id = 700_023i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(depot_1_id, InventoryItemRecord {
            item_id: depot_1_id, type_id: TYPE_MOBILE_DEPOT, owner_id: PILOT_1,
            location_id: SHIP_1, flag: InventoryFlag::Cargo, quantity: 1, singleton: true, ..Default::default()
        });
        snap.items.insert(depot_2_id, InventoryItemRecord {
            item_id: depot_2_id, type_id: TYPE_MOBILE_DEPOT, owner_id: PILOT_1,
            location_id: SHIP_1, flag: InventoryFlag::Cargo, quantity: 1, singleton: true, ..Default::default()
        });
    });
    ctx.dispatch_rpc(&mut sess_valid, "posMgr", None, "AnchorStructure", &[PyRep::Integer(depot_1_id)], &kw, None).await.unwrap();
    let depot_2_res = ctx.dispatch_rpc(&mut sess_valid, "posMgr", None, "AnchorStructure", &[PyRep::Integer(depot_2_id)], &kw, None).await;
    assert!(depot_2_res.is_err());
    assert_eq!(depot_2_res.unwrap_err().code, "DeploymentTooCloseToSameType", "Cannot anchor second depot within 5km of another depot");

    // Proximity to Station (>= 50km) (MD-P1-3)
    let st_pos = ctx.sde.stations.values().find(|s| s.solar_system_id == TEST_SYSTEM).map(|s| s.position).unwrap_or(DVec3::new(100_000.0, 0.0, 0.0));
    let mut station_sess = setup_pilot(&ctx, 307, PILOT_1, SHIP_1, TEST_SYSTEM, st_pos + DVec3::new(10_000.0, 0.0, 0.0)).await;
    let st_depot_id = 700_024i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(st_depot_id, InventoryItemRecord {
            item_id: st_depot_id, type_id: TYPE_MOBILE_DEPOT, owner_id: PILOT_1,
            location_id: SHIP_1, flag: InventoryFlag::Cargo, quantity: 1, singleton: true, ..Default::default()
        });
    });
    let st_res = ctx.dispatch_rpc(&mut station_sess, "posMgr", None, "AnchorStructure", &[PyRep::Integer(st_depot_id)], &kw, None).await;
    assert_eq!(st_res.unwrap_err().code, "DeploymentTooCloseToStation", "Cannot deploy mobile structure within 50km of station");

    // Proximity between two MTUs (>= 5km) (MD-P1-3)
    let mtu_pos1 = valid_pos + DVec3::new(200_000.0, 0.0, 0.0);
    let mut mtu_sess = setup_pilot(&ctx, 308, PILOT_1, SHIP_1, TEST_SYSTEM, mtu_pos1).await;
    let mtu_1_id = 700_025i64;
    let mtu_2_id = 700_026i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(mtu_1_id, InventoryItemRecord {
            item_id: mtu_1_id, type_id: TYPE_MTU_STANDARD, owner_id: PILOT_1,
            location_id: SHIP_1, flag: InventoryFlag::Cargo, quantity: 1, singleton: true, ..Default::default()
        });
        snap.items.insert(mtu_2_id, InventoryItemRecord {
            item_id: mtu_2_id, type_id: TYPE_MTU_STANDARD, owner_id: PILOT_1,
            location_id: SHIP_1, flag: InventoryFlag::Cargo, quantity: 1, singleton: true, ..Default::default()
        });
    });
    ctx.dispatch_rpc(&mut mtu_sess, "posMgr", None, "AnchorStructure", &[PyRep::Integer(mtu_1_id)], &kw, None).await.unwrap();
    let mtu_2_res = ctx.dispatch_rpc(&mut mtu_sess, "posMgr", None, "AnchorStructure", &[PyRep::Integer(mtu_2_id)], &kw, None).await;
    assert_eq!(mtu_2_res.unwrap_err().code, "DeploymentTooCloseToSameType", "Cannot deploy second MTU within 5km of another MTU");

    // 5. Crimewatch Suspect Flag (MD-07): Pilot 2 attacks Pilot 1's anchored depot in highsec through real weapons dispatch
    let mut sess_p2 = setup_pilot(&ctx, 306, PILOT_2, SHIP_2, TEST_SYSTEM, valid_pos + DVec3::new(2_000.0, 0.0, 0.0)).await;
    let turret_mod_id = 800_100i64;
    let ammo_mod_id = 800_101i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(turret_mod_id, InventoryItemRecord {
            item_id: turret_mod_id, type_id: 47272, owner_id: PILOT_2,
            location_id: SHIP_2, flag: eve_db::InventoryFlag::HiSlot0, quantity: 1, singleton: true, ..Default::default()
        });
        snap.items.insert(ammo_mod_id, InventoryItemRecord {
            item_id: ammo_mod_id, type_id: 47925, owner_id: PILOT_2,
            location_id: SHIP_2, flag: eve_db::InventoryFlag::Cargo, quantity: 50, singleton: false, ..Default::default()
        });
    });

    let sys_handle = ctx.systems.get_or_spawn(TEST_SYSTEM);
    sys_handle.add_target_lock(SHIP_2, eve_destiny::TargetLock { target_id: depot_1_id, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(50_000.0) }).await.unwrap();
    let now_attack = ctx.now_unix();
    let fire_res = ctx.dispatch_rpc(
        &mut sess_p2,
        "dogmaIM",
        None,
        "Activate",
        &[PyRep::Integer(turret_mod_id), PyRep::Integer(depot_1_id)],
        &kw,
        None,
    ).await;
    assert!(fire_res.is_ok(), "Firing on mobile deployable in highsec must succeed: {fire_res:?}");

    // Verify Pilot 2 received a SUSPECT flag, NOT a CRIMINAL flag
    let p2_state = ctx.crimewatch.read().unwrap().states.get(&PILOT_2).cloned().expect("p2 cw state");
    assert!(p2_state.is_suspect(now_attack), "Attacking mobile deployable in highsec must trigger suspect flag");
    assert!(!p2_state.is_criminal(now_attack), "Attacking mobile deployable must NOT trigger criminal flag");
}

}

#[allow(dead_code, unused_imports)]
pub(crate) mod mobile_structures_anchoring {
    use super::*;

//  Integration tests for Mobile Tractor Units (`feat:structures.mobile-tractor`, `feat:structures.mobile-tractor-rules`).


use std::collections::HashMap;
use glam::DVec3;
use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, DamageVector, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::mobile_deployables::*;

const PILOT_1: i64 = 90_000_001;
const PILOT_2: i64 = 90_000_002;
const SHIP_1: i64 = 100_000_001;
const TEST_SYSTEM: u32 = 30_000_142; // Jita

async fn setup_pilot(
    ctx: &ServerContext,
    sess_id: u64,
    char_id: i64,
    ship_id: i64,
    sys_id: u32,
    pos: DVec3,
) -> SessionState {
    let mut sess = SessionState::new(sess_id);
    sess.user_id = Some(char_id as u64);
    sess.character_id = Some(char_id);
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);

    ctx.db.with_mut_snapshot(|snap| {
        if !snap.characters.contains_key(&char_id) {
            if let Some(mut tmpl) = snap.characters.values().next().cloned() {
                tmpl.character_id = char_id;
                tmpl.corporation_id = 1_000_002;
                snap.characters.insert(char_id, tmpl);
            }
        }
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = ship_id;
        }
        snap.items.insert(ship_id, InventoryItemRecord {
            item_id: ship_id, type_id: 587, owner_id: char_id, location_id: sys_id as i64,
            flag: InventoryFlag::None, quantity: 1, singleton: true, ..Default::default()
        });
        snap.item_positions.insert(ship_id, pos.to_array());
    });

    let sys = ctx.systems.get_or_spawn(sys_id);
    let ball = Ball {
        id: ship_id, type_id: 587, position: pos, velocity: DVec3::ZERO, max_velocity: 300.0,
        inertia: 1.0, mass: 1_000_000.0, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop,
    };
    let _ = sys.spawn_ball_with_meta(
        ball,
        ctx.build_ship_combat_state(char_id, ship_id, 587),
        ctx.build_ship_slim_meta(char_id, 587),
    ).await;

    sess
}

/// Tests Mobile Tractor Unit automated tractoring within 125km, owner-only wreck targeting,
/// closest-first ordering, hold capacity gating (27,000 m³), and cargo looting into MTU hold
/// (`feat:structures.mobile-tractor`, `feat:structures.mobile-tractor-rules`).
#[tokio::test]
async fn test_mobile_tractor_unit_tractor_and_looting() {
    // parity: feat:structures.mobile-tractor, feat:structures.mobile-tractor-rules
    eve_testkit::attest(&[
        "feat:structures.mobile-tractor",
        "feat:structures.mobile-tractor-rules",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let ship_pos = DVec3::new(40_000_000.0, 40_000_000.0, 40_000_000.0);
    let mut sess1 = setup_pilot(&ctx, 401, PILOT_1, SHIP_1, TEST_SYSTEM, ship_pos).await;

    let mtu_id = 710_001i64;
    ctx.db.with_mut_snapshot(|snap| {
        let skills = snap.skills.entry(PILOT_1).or_default();
        skills.insert(11584, eve_db::TrainedSkillRecord { type_id: 11584, level: 3, skill_points: 40_000.0 });
        snap.items.insert(mtu_id, InventoryItemRecord {
            item_id: mtu_id, type_id: TYPE_MTU_STANDARD, owner_id: PILOT_1,
            location_id: SHIP_1, flag: InventoryFlag::Cargo, quantity: 1, singleton: true, ..Default::default()
        });
    });

    let kw = HashMap::new();
    let res = ctx.dispatch_rpc(
        &mut sess1,
        "posMgr",
        None,
        "AnchorStructure",
        &[PyRep::Integer(mtu_id)],
        &kw,
        None,
    ).await;
    assert!(res.is_ok(), "MTU anchor failed: {res:?}");

    // Advance clock past the MTU anchoring delay
    ctx.advance_secs(15);
    let sys = ctx.systems.get_or_spawn(TEST_SYSTEM);
    sys.step_manual(1.0).await.unwrap();

    // Spawn Pilot 1's wreck at 40km from MTU
    let wreck_owner_id = 720_001i64;
    let wreck_owner_pos = ship_pos + DVec3::new(40_000.0, 0.0, 0.0);
    let item_a_id = 730_001i64;
    let item_b_id = 730_002i64;

    // Spawn Pilot 1's second wreck at 60km from MTU (MD-P1-1)
    let wreck_owner_2_id = 720_004i64;
    let wreck_owner_2_pos = ship_pos + DVec3::new(60_000.0, 0.0, 0.0);
    let item_c_id = 730_004i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(wreck_owner_id, InventoryItemRecord {
            item_id: wreck_owner_id, type_id: 26557, owner_id: PILOT_1,
            location_id: TEST_SYSTEM as i64, flag: InventoryFlag::Cargo, quantity: 1, singleton: true, ..Default::default()
        });
        snap.item_positions.insert(wreck_owner_id, wreck_owner_pos.to_array());
        // Items in wreck 1: Tritanium (0.01 m3) and Pyerite (0.01 m3)
        snap.items.insert(item_a_id, InventoryItemRecord {
            item_id: item_a_id, type_id: 34, owner_id: PILOT_1,
            location_id: wreck_owner_id, flag: InventoryFlag::Cargo, quantity: 100, singleton: false, ..Default::default()
        });
        snap.items.insert(item_b_id, InventoryItemRecord {
            item_id: item_b_id, type_id: 35, owner_id: PILOT_1,
            location_id: wreck_owner_id, flag: InventoryFlag::Cargo, quantity: 50, singleton: false, ..Default::default()
        });

        // Items in wreck 2: Tritanium
        snap.items.insert(wreck_owner_2_id, InventoryItemRecord {
            item_id: wreck_owner_2_id, type_id: 26557, owner_id: PILOT_1,
            location_id: TEST_SYSTEM as i64, flag: InventoryFlag::Cargo, quantity: 1, singleton: true, ..Default::default()
        });
        snap.item_positions.insert(wreck_owner_2_id, wreck_owner_2_pos.to_array());
        snap.items.insert(item_c_id, InventoryItemRecord {
            item_id: item_c_id, type_id: 34, owner_id: PILOT_1,
            location_id: wreck_owner_2_id, flag: InventoryFlag::Cargo, quantity: 75, singleton: false, ..Default::default()
        });
    });
    let _ = sys.spawn_ball(Ball {
        id: wreck_owner_id, type_id: 26557, position: wreck_owner_pos, velocity: DVec3::ZERO,
        max_velocity: 1000.0, inertia: 1.0, mass: 10_000.0, radius: 30.0, speed_fraction: 0.0, mode: MovementMode::Stop,
    }).await;
    let _ = sys.spawn_ball(Ball {
        id: wreck_owner_2_id, type_id: 26557, position: wreck_owner_2_pos, velocity: DVec3::ZERO,
        max_velocity: 1000.0, inertia: 1.0, mass: 10_000.0, radius: 30.0, speed_fraction: 0.0, mode: MovementMode::Stop,
    }).await;

    // Spawn Pilot 2's wreck at 30km from MTU (closer, but not owned by MTU owner)
    let wreck_other_id = 720_002i64;
    let wreck_other_pos = ship_pos + DVec3::new(30_000.0, 0.0, 0.0);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(wreck_other_id, InventoryItemRecord {
            item_id: wreck_other_id, type_id: 26557, owner_id: PILOT_2,
            location_id: TEST_SYSTEM as i64, flag: InventoryFlag::Cargo, quantity: 1, singleton: true, ..Default::default()
        });
        snap.item_positions.insert(wreck_other_id, wreck_other_pos.to_array());
    });
    let _ = sys.spawn_ball(Ball {
        id: wreck_other_id, type_id: 26557, position: wreck_other_pos, velocity: DVec3::ZERO,
        max_velocity: 1000.0, inertia: 1.0, mass: 10_000.0, radius: 30.0, speed_fraction: 0.0, mode: MovementMode::Stop,
    }).await;

    // Step 1: MTU ticks
    sys.step_manual(1.0).await.unwrap();

    // 1. Verify Pilot 2's wreck is NOT tractored (non-owner wrecks are ignored)
    let other_pos = ctx.db.with_snapshot(|snap| snap.item_positions.get(&wreck_other_id).copied()).unwrap();
    assert_eq!(DVec3::from_array(other_pos), wreck_other_pos, "MTU must NOT tractor wrecks belonging to other pilots");

    // 2. Verify Pilot 1's wreck 1 (closest, 40km) was pulled towards MTU by ~1,000m (tractoring speed: 1,000 m/s)
    let owner_pos_step1 = ctx.db.with_snapshot(|snap| snap.item_positions.get(&wreck_owner_id).copied()).unwrap();
    let initial_dist = (wreck_owner_pos - ship_pos).length();
    let post_dist = (DVec3::from_array(owner_pos_step1) - ship_pos).length();
    assert!((initial_dist - post_dist - 1_000.0).abs() < 100.0, "Wreck 1 must be pulled ~1,000m per 1.0s tick");

    // Verify wreck 2 (60km) has not been pulled yet
    let owner_2_pos_step1 = ctx.db.with_snapshot(|snap| snap.item_positions.get(&wreck_owner_2_id).copied()).unwrap();
    assert_eq!(DVec3::from_array(owner_2_pos_step1), wreck_owner_2_pos, "Wreck 2 must not be tractored while Wreck 1 is active");

    // Step until wreck 1 arrives within 2,500m of MTU
    for _ in 0..40 {
        sys.step_manual(1.0).await.unwrap();
    }

    // 3. Verify wreck 1 items are transferred into the MTU cargo hold
    let item_a_loc = ctx.db.with_snapshot(|snap| snap.items.get(&item_a_id).map(|i| i.location_id));
    let item_b_loc = ctx.db.with_snapshot(|snap| snap.items.get(&item_b_id).map(|i| i.location_id));
    assert_eq!(item_a_loc, Some(mtu_id), "Item A must be looted into MTU cargo hold");
    assert_eq!(item_b_loc, Some(mtu_id), "Item B must be looted into MTU cargo hold");

    // 4. MTU now moves on to tractor wreck 2 (60km) (MD-P1-1)
    let pre_w2_pos = ctx.db.with_snapshot(|snap| snap.item_positions.get(&wreck_owner_2_id).copied()).unwrap();
    sys.step_manual(1.0).await.unwrap();
    let post_w2_pos = ctx.db.with_snapshot(|snap| snap.item_positions.get(&wreck_owner_2_id).copied()).unwrap();
    let w2_dist_pre = (DVec3::from_array(pre_w2_pos) - ship_pos).length();
    let w2_dist_post = (DVec3::from_array(post_w2_pos) - ship_pos).length();
    assert!((w2_dist_pre - w2_dist_post - 1_000.0).abs() < 100.0, "Wreck 2 must now be pulled ~1,000m per tick");

    // Step until wreck 2 arrives within 2,500m of MTU and is looted
    for _ in 0..60 {
        sys.step_manual(1.0).await.unwrap();
    }
    let item_c_loc = ctx.db.with_snapshot(|snap| snap.items.get(&item_c_id).map(|i| i.location_id));
    assert_eq!(item_c_loc, Some(mtu_id), "Item C must be looted into MTU cargo hold");

    // 4. Hold Capacity check: spawn an item that exceeds MTU capacity (27,000 m³)
    let huge_wreck_id = 720_003i64;
    let huge_item_id = 730_003i64;
    let near_wreck_pos = ship_pos + DVec3::new(1_000.0, 0.0, 0.0);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(huge_wreck_id, InventoryItemRecord {
            item_id: huge_wreck_id, type_id: 26557, owner_id: PILOT_1,
            location_id: TEST_SYSTEM as i64, flag: InventoryFlag::Cargo, quantity: 1, singleton: true, ..Default::default()
        });
        snap.item_positions.insert(huge_wreck_id, near_wreck_pos.to_array());
        // Type 20185 (Freighter hull) volume is ~1,000,000 m3
        snap.items.insert(huge_item_id, InventoryItemRecord {
            item_id: huge_item_id, type_id: 20185, owner_id: PILOT_1,
            location_id: huge_wreck_id, flag: InventoryFlag::Cargo, quantity: 1, singleton: true, ..Default::default()
        });
    });
    sys.step_manual(1.0).await.unwrap();

    let huge_loc = ctx.db.with_snapshot(|snap| snap.items.get(&huge_item_id).map(|i| i.location_id));
    assert_eq!(huge_loc, Some(huge_wreck_id), "Item exceeding MTU capacity must NOT be looted into MTU hold");
}

/// Tests Mobile Tractor Unit idle state when owner is absent, 2-day decay lifetime,
/// and 50% random cargo loot drop on destruction (`feat:structures.mobile-tractor-rules`).
#[tokio::test]
async fn test_mobile_tractor_unit_rules_and_destruction() {
    // parity: feat:structures.mobile-tractor-rules
    eve_testkit::attest(&["feat:structures.mobile-tractor-rules"]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let _start_unix = ctx.now_unix();
    let ship_pos = DVec3::new(50_000_000.0, 50_000_000.0, 50_000_000.0);
    let mut sess = setup_pilot(&ctx, 501, PILOT_1, SHIP_1, TEST_SYSTEM, ship_pos).await;

    let mtu_id = 710_010i64;
    ctx.db.with_mut_snapshot(|snap| {
        let skills = snap.skills.entry(PILOT_1).or_default();
        skills.insert(11584, eve_db::TrainedSkillRecord { type_id: 11584, level: 3, skill_points: 40_000.0 });
        snap.items.insert(mtu_id, InventoryItemRecord {
            item_id: mtu_id, type_id: TYPE_MTU_STANDARD, owner_id: PILOT_1,
            location_id: SHIP_1, flag: InventoryFlag::Cargo, quantity: 1, singleton: true, ..Default::default()
        });
    });

    let kw = HashMap::new();
    ctx.dispatch_rpc(&mut sess, "posMgr", None, "AnchorStructure", &[PyRep::Integer(mtu_id)], &kw, None).await.unwrap();
    ctx.advance_secs(15);
    let sys = ctx.systems.get_or_spawn(TEST_SYSTEM);
    sys.step_manual(1.0).await.unwrap();

    // Spawn a wreck at 20km with items
    let test_wreck_id = 720_010i64;
    let test_wreck_pos = ship_pos + DVec3::new(20_000.0, 0.0, 0.0);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(test_wreck_id, InventoryItemRecord {
            item_id: test_wreck_id, type_id: 26557, owner_id: PILOT_1,
            location_id: TEST_SYSTEM as i64, flag: InventoryFlag::Cargo, quantity: 1, singleton: true, ..Default::default()
        });
        snap.item_positions.insert(test_wreck_id, test_wreck_pos.to_array());
        snap.items.insert(730_010i64, InventoryItemRecord {
            item_id: 730_010i64, type_id: 34, owner_id: PILOT_1,
            location_id: test_wreck_id, flag: InventoryFlag::Cargo, quantity: 100, singleton: false, ..Default::default()
        });
    });
    let _ = sys.spawn_ball(Ball {
        id: test_wreck_id, type_id: 26557, position: test_wreck_pos, velocity: DVec3::ZERO,
        max_velocity: 1000.0, inertia: 1.0, mass: 10_000.0, radius: 30.0, speed_fraction: 0.0, mode: MovementMode::Stop,
    }).await;

    // 1. Owner leaves system (docks in station)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_1) {
            ch.station_id = Some(60003760);
        }
    });
    sys.step_manual(1.0).await.unwrap();
    let pos_docked = ctx.db.with_snapshot(|snap| snap.item_positions.get(&test_wreck_id).copied()).unwrap();
    assert_eq!(DVec3::from_array(pos_docked), test_wreck_pos, "MTU must be idle when owner is docked or not in space");

    // Owner undocks back to system
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_1) {
            ch.station_id = None;
        }
    });
    sys.step_manual(1.0).await.unwrap();
    let pos_undocked = ctx.db.with_snapshot(|snap| snap.item_positions.get(&test_wreck_id).copied()).unwrap();
    assert_ne!(DVec3::from_array(pos_undocked), test_wreck_pos, "MTU must resume tractoring when owner is active in system");

    // 2. MTU 2-day decay lifetime
    let mtu_decay_id = 710_020i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(mtu_decay_id, InventoryItemRecord {
            item_id: mtu_decay_id, type_id: TYPE_MTU_STANDARD, owner_id: PILOT_1,
            location_id: SHIP_1, flag: InventoryFlag::Cargo, quantity: 1, singleton: true, ..Default::default()
        });
    });
    let decay_deploy_pos = ship_pos + DVec3::new(10_000.0, 10_000.0, 0.0);
    ctx.db.with_mut_snapshot(|snap| snap.item_positions.insert(SHIP_1, decay_deploy_pos.to_array()));
    let _ = sys.set_ball_position(SHIP_1, decay_deploy_pos).await;
    ctx.dispatch_rpc(&mut sess, "posMgr", None, "AnchorStructure", &[PyRep::Integer(mtu_decay_id)], &kw, None).await.unwrap();
    ctx.advance_secs(15);
    sys.step_manual(1.0).await.unwrap();
    assert!(sys.get_combat_state(mtu_decay_id).await.unwrap().is_some(), "MTU must be active after anchoring");

    // Advance clock past 2 days (172,800s)
    ctx.advance_secs(MTU_LIFETIME_SECS + 1);
    sys.step_manual(1.0).await.unwrap();
    assert!(sys.get_combat_state(mtu_decay_id).await.unwrap().is_none(), "MTU must decay and despawn after 2 days");
    let decay_in_db = ctx.db.with_snapshot(|snap| snap.items.contains_key(&mtu_decay_id));
    assert!(!decay_in_db, "Decayed MTU item must be removed from DB");

    // 3. MTU Destruction Loot Drop: drops 50% random loot into a cargo container
    let mtu_loot_id = 710_030i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(mtu_loot_id, InventoryItemRecord {
            item_id: mtu_loot_id, type_id: TYPE_MTU_STANDARD, owner_id: PILOT_1,
            location_id: SHIP_1, flag: InventoryFlag::Cargo, quantity: 1, singleton: true, ..Default::default()
        });
    });
    let loot_deploy_pos = ship_pos + DVec3::new(20_000.0, 20_000.0, 0.0);
    ctx.db.with_mut_snapshot(|snap| snap.item_positions.insert(SHIP_1, loot_deploy_pos.to_array()));
    let _ = sys.set_ball_position(SHIP_1, loot_deploy_pos).await;
    ctx.dispatch_rpc(&mut sess, "posMgr", None, "AnchorStructure", &[PyRep::Integer(mtu_loot_id)], &kw, None).await.unwrap();
    ctx.advance_secs(15);
    sys.step_manual(1.0).await.unwrap();

    // Populate 6 cargo items inside the MTU hold
    let loot_ids: Vec<i64> = (1..=6).map(|i| 740_000 + i).collect();
    ctx.db.with_mut_snapshot(|snap| {
        for &lid in &loot_ids {
            snap.items.insert(lid, InventoryItemRecord {
                item_id: lid, type_id: 34, owner_id: PILOT_1,
                location_id: mtu_loot_id, flag: InventoryFlag::Cargo, quantity: 100, singleton: false, ..Default::default()
            });
        }
    });

    // Destroy MTU with heavy damage
    let fatal_dmg = DamageVector { em: 50_000.0, thermal: 50_000.0, kinetic: 50_000.0, explosive: 50_000.0 };
    let (_, destroyed, _) = sys.apply_damage(mtu_loot_id, fatal_dmg).await.unwrap().expect("hit");
    assert!(destroyed, "MTU must be destroyed by lethal damage");

    // Verify container spawned at MTU position holding dropped loot
    let (container_found, dropped_count) = ctx.db.with_snapshot(|snap| {
        let cans: Vec<i64> = snap.items.values()
            .filter(|i| i.type_id == 23 && i.location_id == TEST_SYSTEM as i64)
            .map(|i| i.item_id)
            .collect();
        let mut drop_total = 0;
        for &cid in &cans {
            let inside = snap.items.values().filter(|i| i.location_id == cid).count();
            if inside > 0 {
                drop_total += inside;
            }
        }
        (!cans.is_empty(), drop_total)
    });
    assert!(container_found, "Cargo container must spawn upon MTU destruction");
    assert!(dropped_count > 0, "Some items must drop into the container per the 50% roll");

    // MD-P0-1 Regression: verify record removed from st.deployables upon MTU destruction
    let dep_st = read_deployable_state(&ctx);
    assert!(!dep_st.deployables.contains_key(&mtu_loot_id), "Destroyed MTU must be removed from deployable runtime state");

    // Spawn an owner wreck with contents at 20km from destroyed MTU
    let after_wreck_id = 720_099i64;
    let after_wreck_pos = loot_deploy_pos + DVec3::new(20_000.0, 0.0, 0.0);
    let after_item_id = 730_099i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(after_wreck_id, InventoryItemRecord {
            item_id: after_wreck_id, type_id: 26557, owner_id: PILOT_1,
            location_id: TEST_SYSTEM as i64, flag: InventoryFlag::Cargo, quantity: 1, singleton: true, ..Default::default()
        });
        snap.item_positions.insert(after_wreck_id, after_wreck_pos.to_array());
        snap.items.insert(after_item_id, InventoryItemRecord {
            item_id: after_item_id, type_id: 34, owner_id: PILOT_1,
            location_id: after_wreck_id, flag: InventoryFlag::Cargo, quantity: 50, singleton: false, ..Default::default()
        });
    });
    let _ = sys.spawn_ball(Ball {
        id: after_wreck_id, type_id: 26557, position: after_wreck_pos, velocity: DVec3::ZERO,
        max_velocity: 1000.0, inertia: 1.0, mass: 10_000.0, radius: 30.0, speed_fraction: 0.0, mode: MovementMode::Stop,
    }).await;

    // Step system for 5 ticks
    for _ in 0..5 {
        sys.step_manual(1.0).await.unwrap();
    }

    // Assert wreck position does NOT change and contents are not looted
    let cur_pos = ctx.db.with_snapshot(|snap| snap.item_positions.get(&after_wreck_id).copied()).unwrap();
    assert_eq!(DVec3::from_array(cur_pos), after_wreck_pos, "Destroyed MTU must not tractor any wrecks");
    let cur_loc = ctx.db.with_snapshot(|snap| snap.items.get(&after_item_id).map(|i| i.location_id));
    assert_eq!(cur_loc, Some(after_wreck_id), "Contents must never be looted into destroyed MTU");
}

}

#[allow(dead_code, unused_imports)]
pub(crate) mod mobile_structures_defense {
    use super::*;

//  Integration tests for Mobile Deployable lifecycles, cluster tick decay, multi-reinforcement,
//  and Cyno Inhibitor restrictions (`feat:structures.mobile-depot`, `feat:structures.mobile-depot-lifetime-reinforce`, `feat:structures.mobile-deploy-restrictions`).


use std::collections::HashMap;
use glam::DVec3;
use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, DamageVector, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::inventory_fitting::fitting_containers_ships::validate_single_item_can_be_opened;
use eve_server::services::mobile_deployables::*;
use eve_server::sp6_lifecycle::tick_background_cluster;

const PILOT_1: i64 = 90_000_001;
const PILOT_2: i64 = 90_000_002;
const SHIP_1: i64 = 100_000_001;
const SHIP_2: i64 = 100_000_002;
const TEST_SYSTEM: u32 = 30_000_142; // Jita

async fn setup_pilot(
    ctx: &ServerContext,
    sess_id: u64,
    char_id: i64,
    ship_id: i64,
    sys_id: u32,
    pos: DVec3,
) -> SessionState {
    let mut sess = SessionState::new(sess_id);
    sess.user_id = Some(char_id as u64);
    sess.character_id = Some(char_id);
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);

    ctx.db.with_mut_snapshot(|snap| {
        if !snap.characters.contains_key(&char_id) {
            if let Some(mut tmpl) = snap.characters.values().next().cloned() {
                tmpl.character_id = char_id;
                tmpl.corporation_id = 1_000_002;
                tmpl.security_status = 5.0;
                snap.characters.insert(char_id, tmpl);
            }
        }
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = ship_id;
            ch.security_status = 5.0;
        }
        snap.items.insert(ship_id, InventoryItemRecord {
            item_id: ship_id, type_id: 587, owner_id: char_id, location_id: sys_id as i64,
            flag: InventoryFlag::None, quantity: 1, singleton: true, ..Default::default()
        });
        snap.item_positions.insert(ship_id, pos.to_array());
    });

    let sys = ctx.systems.get_or_spawn(sys_id);
    let ball = Ball {
        id: ship_id, type_id: 587, position: pos, velocity: DVec3::ZERO, max_velocity: 300.0,
        inertia: 1.0, mass: 1_000_000.0, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop,
    };
    let _ = sys.spawn_ball_with_meta(
        ball,
        ctx.build_ship_combat_state(char_id, ship_id, 587),
        ctx.build_ship_slim_meta(char_id, 587),
    ).await;

    sess
}

/// Tests Mobile Depot cluster tick decay vs 30-day lifetime refresh and expiry (`feat:structures.mobile-depot-lifetime-reinforce`).
#[tokio::test]
async fn test_mobile_depot_cluster_tick_decay_vs_30day_lifetime() {
    // parity: feat:structures.mobile-depot-lifetime-reinforce
    eve_testkit::attest(&["feat:structures.mobile-depot-lifetime-reinforce"]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let start_unix = ctx.now_unix();
    let ship_pos = DVec3::new(60_000_000.0, 60_000_000.0, 60_000_000.0);
    let mut sess = setup_pilot(&ctx, 601, PILOT_1, SHIP_1, TEST_SYSTEM, ship_pos).await;

    let depot_id = 700_040i64;
    ctx.db.with_mut_snapshot(|snap| {
        let skills = snap.skills.entry(PILOT_1).or_default();
        skills.insert(11584, eve_db::TrainedSkillRecord { type_id: 11584, level: 3, skill_points: 40_000.0 });
        snap.items.insert(depot_id, InventoryItemRecord {
            item_id: depot_id, type_id: TYPE_MOBILE_DEPOT, owner_id: PILOT_1,
            location_id: SHIP_1, flag: InventoryFlag::Cargo, quantity: 1, singleton: true, ..Default::default()
        });
    });

    let kw = HashMap::new();
    ctx.dispatch_rpc(&mut sess, "posMgr", None, "AnchorStructure", &[PyRep::Integer(depot_id)], &kw, None).await.unwrap();

    let rec0 = read_deployable_state(&ctx).deployables.get(&depot_id).cloned().expect("depot record");
    assert_eq!(rec0.expires_at_unix, start_unix + DEPOT_LIFETIME_SECS);

    // Advance clock past 2 hours (7,300s > legacy 7,200s secure container decay window)
    ctx.advance_secs(7_300);
    tick_background_cluster(&ctx).await;

    // Verify depot is STILL alive in space (NOT deleted by legacy 2-hour container decay sweep)
    assert!(ctx.db.with_snapshot(|s| s.items.contains_key(&depot_id)), "Mobile Depot must NOT be deleted by legacy 2h container decay sweep");
    assert!(read_deployable_state(&ctx).deployables.contains_key(&depot_id), "Mobile Depot record must persist");

    // Interact with depot -> resets lifetime to 30 days from now
    validate_single_item_can_be_opened(&ctx, PILOT_1, depot_id).await.expect("open depot");
    let rec_refreshed = read_deployable_state(&ctx).deployables.get(&depot_id).cloned().expect("rec");
    let now_interact = ctx.now_unix();
    assert_eq!(rec_refreshed.expires_at_unix, now_interact.saturating_add(DEPOT_LIFETIME_SECS));

    // Advance cluster tick past 30 days
    ctx.advance_secs(DEPOT_LIFETIME_SECS + 1);
    tick_background_cluster(&ctx).await;

    // Verify it cleanly expires
    assert!(!ctx.db.with_snapshot(|s| s.items.contains_key(&depot_id)), "Mobile Depot must cleanly expire after 30 days");
    assert!(!read_deployable_state(&ctx).deployables.contains_key(&depot_id), "Mobile Depot record must be removed on expiry");
}

/// Tests Mobile Depot multi-reinforcement upon shield regeneration above 25% (`feat:structures.mobile-depot`, `feat:structures.mobile-depot-lifetime-reinforce`).
#[tokio::test]
async fn test_mobile_depot_multi_reinforcement_after_shield_regen() {
    // parity: feat:structures.mobile-depot, feat:structures.mobile-depot-lifetime-reinforce
    eve_testkit::attest(&[
        "feat:structures.mobile-depot",
        "feat:structures.mobile-depot-lifetime-reinforce",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let ship_pos = DVec3::new(65_000_000.0, 65_000_000.0, 65_000_000.0);
    let mut sess = setup_pilot(&ctx, 602, PILOT_1, SHIP_1, TEST_SYSTEM, ship_pos).await;

    let depot_id = 700_050i64;
    ctx.db.with_mut_snapshot(|snap| {
        let skills = snap.skills.entry(PILOT_1).or_default();
        skills.insert(11584, eve_db::TrainedSkillRecord { type_id: 11584, level: 3, skill_points: 40_000.0 });
        snap.items.insert(depot_id, InventoryItemRecord {
            item_id: depot_id, type_id: TYPE_MOBILE_DEPOT, owner_id: PILOT_1,
            location_id: SHIP_1, flag: InventoryFlag::Cargo, quantity: 1, singleton: true, ..Default::default()
        });
    });

    let kw = HashMap::new();
    ctx.dispatch_rpc(&mut sess, "posMgr", None, "AnchorStructure", &[PyRep::Integer(depot_id)], &kw, None).await.unwrap();
    ctx.advance_secs(DEPOT_ANCHOR_TIME_SECS + 1);
    let sys = ctx.systems.get_or_spawn(TEST_SYSTEM);
    sys.step_manual(1.0).await.unwrap();

    let d_cs = sys.get_combat_state(depot_id).await.unwrap().expect("combat state");
    let threshold = d_cs.max_shield_hp * DEPOT_REINFORCE_SHIELD_THRESHOLD;

    // Attack 1: bring shields across 25% threshold -> enters 1st reinforcement
    let dmg1 = DamageVector { em: d_cs.shield_hp - threshold + 500.0, thermal: 0.0, kinetic: 0.0, explosive: 0.0 };
    let (applied1, destroyed1, _) = sys.apply_damage(depot_id, dmg1).await.unwrap().unwrap();
    assert!(!destroyed1);
    assert_eq!(applied1, d_cs.shield_hp - threshold);
    let rec1 = read_deployable_state(&ctx).deployables.get(&depot_id).cloned().unwrap();
    assert!(rec1.reinforced_until_unix.is_some(), "Depot must enter reinforcement");

    // Advance clock past 48-hour reinforcement duration
    ctx.advance_secs(DEPOT_REINFORCE_DURATION_SECS + 1);
    sys.step_manual(1.0).await.unwrap();

    // Verify reinforcement timer is cleared
    let rec_post = read_deployable_state(&ctx).deployables.get(&depot_id).cloned().unwrap();
    assert!(rec_post.reinforced_until_unix.is_none(), "Reinforcement timer must be cleared after duration");

    // Regenerate shields above 25% threshold
    let mut cs_to_regen = sys.get_combat_state(depot_id).await.unwrap().unwrap();
    cs_to_regen.shield_hp = cs_to_regen.max_shield_hp * 0.75;
    sys.set_combat_state(depot_id, cs_to_regen).await.unwrap();

    let cs_regen = sys.get_combat_state(depot_id).await.unwrap().unwrap();
    assert!(cs_regen.shield_hp > threshold, "Shields must be above 25% threshold");

    // Attack 2: bring shields across 25% threshold a second time -> enters 2nd reinforcement!
    let dmg2 = DamageVector { em: cs_regen.shield_hp - threshold + 300.0, thermal: 0.0, kinetic: 0.0, explosive: 0.0 };
    let (applied2, destroyed2, _) = sys.apply_damage(depot_id, dmg2).await.unwrap().unwrap();
    assert!(!destroyed2);
    assert_eq!(applied2, cs_regen.shield_hp - threshold);
    let rec2 = read_deployable_state(&ctx).deployables.get(&depot_id).cloned().unwrap();
    assert!(rec2.reinforced_until_unix.is_some(), "Depot must enter reinforcement a SECOND time upon shield regeneration");
}

/// Tests Mobile Cyno Inhibitor (group 1249, type 33476) deploy restrictions and suspect-only crimewatch rules (`feat:structures.mobile-deploy-restrictions`).
#[tokio::test]
async fn test_mobile_cyno_inhibitor_deploy_restrictions_and_suspect_flag() {
    // parity: feat:structures.mobile-deploy-restrictions
    eve_testkit::attest(&["feat:structures.mobile-deploy-restrictions"]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let ship_pos = DVec3::new(70_000_000.0, 70_000_000.0, 70_000_000.0);
    let kw = HashMap::new();

    // 1. Starter / Career agent system ban (RestrictionsInEffect)
    let starter_sys = ctx.sde.schools.values().find(|s| (11..=22).contains(&s.school_id)).map(|s| s.solar_system_id).unwrap_or(30000140);
    let mci_starter_id = 700_060i64;
    let mci_type_id = 33476; // Mobile Cynosural Inhibitor (group 1249)
    ctx.db.with_mut_snapshot(|snap| {
        let skills = snap.skills.entry(PILOT_1).or_default();
        skills.insert(11584, eve_db::TrainedSkillRecord { type_id: 11584, level: 5, skill_points: 256_000.0 });
        snap.items.insert(mci_starter_id, InventoryItemRecord {
            item_id: mci_starter_id, type_id: mci_type_id, owner_id: PILOT_1,
            location_id: SHIP_1, flag: InventoryFlag::Cargo, quantity: 1, singleton: true, ..Default::default()
        });
    });
    let mut starter_sess = setup_pilot(&ctx, 603, PILOT_1, SHIP_1, starter_sys, ship_pos).await;
    let starter_res = ctx.dispatch_rpc(&mut starter_sess, "posMgr", None, "AnchorStructure", &[PyRep::Integer(mci_starter_id)], &kw, None).await;
    assert_eq!(starter_res.unwrap_err().code, "RestrictionsInEffect", "Mobile Cyno Inhibitor must be blocked in starter systems");

    // 2. High-sec deployment ban (CantAnchorInHighSecOrWormhole)
    let mci_hs_id = 700_064i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(mci_hs_id, InventoryItemRecord {
            item_id: mci_hs_id, type_id: mci_type_id, owner_id: PILOT_1,
            location_id: SHIP_1, flag: InventoryFlag::Cargo, quantity: 1, singleton: true, ..Default::default()
        });
    });
    let mut hs_sess = setup_pilot(&ctx, 608, PILOT_1, SHIP_1, TEST_SYSTEM, ship_pos).await;
    let hs_res = ctx.dispatch_rpc(&mut hs_sess, "posMgr", None, "AnchorStructure", &[PyRep::Integer(mci_hs_id)], &kw, None).await;
    assert_eq!(hs_res.unwrap_err().code, "CantAnchorInHighSecOrWormhole", "Mobile Cyno Inhibitor must be blocked in high-sec");

    // 3. Proximity to Stargate (>= 50km) in low-sec Rancer (30002718)
    const LOWSEC_SYS: u32 = 30_002_718;
    let gate_pos = ctx.sde.stargates().values().find(|g| g.from_solar_system_id == LOWSEC_SYS).map(|g| g.position).unwrap_or(DVec3::ZERO);
    let mci_gate_id = 700_061i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(mci_gate_id, InventoryItemRecord {
            item_id: mci_gate_id, type_id: mci_type_id, owner_id: PILOT_1,
            location_id: SHIP_1, flag: InventoryFlag::Cargo, quantity: 1, singleton: true, ..Default::default()
        });
    });
    let mut gate_sess = setup_pilot(&ctx, 604, PILOT_1, SHIP_1, LOWSEC_SYS, gate_pos + DVec3::new(10_000.0, 0.0, 0.0)).await;
    let gate_res = ctx.dispatch_rpc(&mut gate_sess, "posMgr", None, "AnchorStructure", &[PyRep::Integer(mci_gate_id)], &kw, None).await;
    assert_eq!(gate_res.unwrap_err().code, "DeploymentTooCloseToGate", "Mobile Cyno Inhibitor must be blocked within 50km of stargates");

    // 4. Proximity to Station (>= 50km) in low-sec Rancer
    let st_pos = ctx.sde.stations.values().find(|s| s.solar_system_id == LOWSEC_SYS).map(|s| s.position).unwrap_or(DVec3::new(100_000.0, 0.0, 0.0));
    let mci_station_id = 700_062i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(mci_station_id, InventoryItemRecord {
            item_id: mci_station_id, type_id: mci_type_id, owner_id: PILOT_1,
            location_id: SHIP_1, flag: InventoryFlag::Cargo, quantity: 1, singleton: true, ..Default::default()
        });
    });
    let mut station_sess = setup_pilot(&ctx, 605, PILOT_1, SHIP_1, LOWSEC_SYS, st_pos + DVec3::new(10_000.0, 0.0, 0.0)).await;
    let station_res = ctx.dispatch_rpc(&mut station_sess, "posMgr", None, "AnchorStructure", &[PyRep::Integer(mci_station_id)], &kw, None).await;
    assert_eq!(station_res.unwrap_err().code, "DeploymentTooCloseToStation", "Mobile Cyno Inhibitor must be blocked within 50km of stations");

    // 5. Valid deployment in low-sec & Crimewatch suspect flag upon attack
    let valid_pos = DVec3::new(80_000_000.0, 80_000_000.0, 80_000_000.0);
    let mci_valid_id = 700_063i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(mci_valid_id, InventoryItemRecord {
            item_id: mci_valid_id, type_id: mci_type_id, owner_id: PILOT_1,
            location_id: SHIP_1, flag: InventoryFlag::Cargo, quantity: 1, singleton: true, ..Default::default()
        });
    });
    let mut valid_sess = setup_pilot(&ctx, 606, PILOT_1, SHIP_1, LOWSEC_SYS, valid_pos).await;
    let anchor_res = ctx.dispatch_rpc(&mut valid_sess, "posMgr", None, "AnchorStructure", &[PyRep::Integer(mci_valid_id)], &kw, None).await;
    assert!(anchor_res.is_ok(), "Mobile Cyno Inhibitor anchoring in valid low-sec space must succeed: {anchor_res:?}");

    // Pilot 2 attacks Pilot 1's Mobile Cyno Inhibitor
    let mut sess_p2 = setup_pilot(&ctx, 607, PILOT_2, SHIP_2, LOWSEC_SYS, valid_pos + DVec3::new(2_000.0, 0.0, 0.0)).await;
    let turret_id = 800_200i64;
    let ammo_id = 800_201i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(turret_id, InventoryItemRecord {
            item_id: turret_id, type_id: 47272, owner_id: PILOT_2,
            location_id: SHIP_2, flag: eve_db::InventoryFlag::HiSlot0, quantity: 1, singleton: true, ..Default::default()
        });
        snap.items.insert(ammo_id, InventoryItemRecord {
            item_id: ammo_id, type_id: 47925, owner_id: PILOT_2,
            location_id: SHIP_2, flag: eve_db::InventoryFlag::Cargo, quantity: 50, singleton: false, ..Default::default()
        });
    });

    let sys_handle = ctx.systems.get_or_spawn(LOWSEC_SYS);
    sys_handle.add_target_lock(SHIP_2, eve_destiny::TargetLock { target_id: mci_valid_id, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(50_000.0) }).await.unwrap();
    let now_attack = ctx.now_unix();
    let fire_res = ctx.dispatch_rpc(
        &mut sess_p2,
        "dogmaIM",
        None,
        "Activate",
        &[PyRep::Integer(turret_id), PyRep::Integer(mci_valid_id)],
        &kw,
        None,
    ).await;
    assert!(fire_res.is_ok(), "Attacking mobile deployable must succeed: {fire_res:?}");

    // Verify Pilot 2 received SUSPECT flag, NOT CRIMINAL flag
    let p2_state = ctx.crimewatch.read().unwrap().states.get(&PILOT_2).cloned().expect("p2 cw state");
    assert!(p2_state.is_suspect(now_attack), "Attacking Mobile Cyno Inhibitor must trigger suspect flag");
    assert!(!p2_state.is_criminal(now_attack), "Attacking Mobile Cyno Inhibitor must NOT trigger criminal flag");
}

}

#[allow(dead_code, unused_imports)]
pub(crate) mod mobile_structures_deployment {
    use super::*;

//  End-to-end V24 RPC integration tests for Step 4B Mobile Deployable Structures (Part 4):
//  - `feat:structures.mobile-cyno-beacon` (`57319`, `59630`)
//  - `feat:structures.mobile-micro-jump-unit` (`33591`)


use eve_db::{InventoryFlag, InventoryItemRecord, TrainedSkillRecord};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use glam::DVec3;
use std::collections::HashMap;

pub(crate) const LOWSEC_AMAMAKE: u32 = 30_002_537;
pub(crate) const LOWSEC_VARD: u32 = 30_002_538;
pub(crate) const HIGHSEC_JITA: u32 = 30_000_142;

pub(crate) async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let kw = HashMap::new();
    ctx.dispatch_rpc(sess, service, None, method, args, &kw, None)
        .await
}

pub(crate) async fn rpc_bound(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    bound_id: Option<u64>,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let kw = HashMap::new();
    ctx.dispatch_rpc(sess, service, bound_id, method, args, &kw, None)
        .await
}

pub(crate) fn kv_get<'a>(rep: &'a PyRep, key: &str) -> Option<&'a PyRep> {
    match rep {
        PyRep::KeyVal(m) => m.get(key),
        PyRep::Dict(entries) => entries.iter().find(|(k, _)| k.as_str() == Some(key)).map(|(_, v)| v),
        _ => None,
    }
}

pub(crate) async fn spawn_pilot_in_space(
    ctx: &ServerContext,
    sess_id: u64,
    char_id: i64,
    corp_id: i64,
    ship_id: i64,
    ship_type_id: u32,
    sys_id: u32,
    pos: DVec3,
) -> SessionState {
    ctx.db.with_mut_snapshot(|snap| {
        if !snap.characters.contains_key(&char_id) {
            let uname = format!("pilot_{char_id}");
            snap.accounts.insert(
                uname.clone(),
                eve_db::AccountRecord {
                    user_id: char_id as u64,
                    username: uname,
                    password_token: String::new(),
                    role: 0,
                    banned_until_unix: None,
                    ban_reason: None,
                },
            );
            let mut tmpl = snap.characters[&90_000_002].clone();
            tmpl.user_id = char_id as u64;
            tmpl.character_id = char_id;
            tmpl.corporation_id = corp_id;
            tmpl.name = format!("Pilot {char_id}");
            snap.characters.insert(char_id, tmpl);
        }
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.clone_grade = 2;
        ch.corporation_id = corp_id;
        ch.solar_system_id = sys_id;
        ch.station_id = None;
        ch.structure_id = None;
        ch.active_ship_id = ship_id;
        ch.jump_cooldown_until_unix = 0;
        ch.jump_fatigue_until_unix = 0;
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: ship_type_id,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.item_positions.insert(ship_id, pos.to_array());
    });
    let uid = ctx.db.with_snapshot(|s| s.characters[&char_id].user_id);
    let _ = ctx
        .db
        .acquire_online_lock_with_computer_hash(uid, sess_id, Some(char_id), None, true, ctx.now_unix());

    let (mass, radius) = ctx
        .sde
        .types
        .get(&ship_type_id)
        .map(|t| (t.mass.max(1_000_000.0), t.radius.max(40.0)))
        .unwrap_or((10_000_000.0, 100.0));
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_h
        .spawn_ball_with_meta(
            Ball {
                id: ship_id,
                type_id: ship_type_id,
                position: pos,
                velocity: DVec3::new(100.0, 0.0, 0.0),
                max_velocity: 250.0,
                inertia: 3.0,
                mass,
                radius,
                speed_fraction: 0.4,
                mode: MovementMode::GotoDirection {
                    direction: DVec3::X,
                },
            },
            ctx.build_ship_combat_state(char_id, ship_id, ship_type_id),
            ctx.build_ship_slim_meta(char_id, ship_type_id),
        )
        .await;

    let mut sess = SessionState::new(sess_id);
    sess.user_id = Some(uid);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);
    sess
}

pub(crate) fn grant_skills(ctx: &ServerContext, char_id: i64, skills: &[(u32, u8)]) {
    ctx.db.with_mut_snapshot(|snap| {
        let map = snap.skills.entry(char_id).or_default();
        for &(sid, lvl) in skills {
            map.insert(
                sid,
                TrainedSkillRecord {
                    type_id: sid,
                    level: lvl,
                    skill_points: 256_000.0,
                },
            );
        }
    });
}

#[tokio::test]
async fn test_mobile_cyno_beacon_and_covert_mobile_cyno_beacon_lifecycle_and_fleet_jump() {
    // parity: feat:structures.mobile-cyno-beacon
    eve_testkit::attest(&["feat:structures.mobile-cyno-beacon"]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let deep_pos = DVec3::new(80_000_000.0, 80_000_000.0, 80_000_000.0);

    let owner_cid = 90_000_201;
    let fleet_cap_cid = 90_000_202;
    let fleet_blops_cid = 90_000_203;
    let nonfleet_corp_cid = 90_000_204;
    let corp_id = 98_002_200i64;

    let mut owner_sess =
        spawn_pilot_in_space(&ctx, 701, owner_cid, corp_id, 810_001, 626, LOWSEC_VARD, deep_pos).await;
    let mut cap_sess =
        spawn_pilot_in_space(&ctx, 702, fleet_cap_cid, corp_id, 810_002, 23757, LOWSEC_AMAMAKE, deep_pos).await;
    let mut blops_sess =
        spawn_pilot_in_space(&ctx, 703, fleet_blops_cid, corp_id, 810_003, 22428, LOWSEC_AMAMAKE, deep_pos).await;
    let mut nonfleet_sess =
        spawn_pilot_in_space(&ctx, 704, nonfleet_corp_cid, corp_id, 810_004, 23757, LOWSEC_AMAMAKE, deep_pos).await;

    let std_beacon_id = 820_001i64;
    let cov_beacon_id = 820_002i64;
    let hs_beacon_id = 820_003i64;
    ctx.db.with_mut_snapshot(|snap| {
        for (iid, tid, loc_ship) in [
            (std_beacon_id, 57319u32, 810_001i64),
            (cov_beacon_id, 59630u32, 810_001i64),
            (hs_beacon_id, 57319u32, 810_001i64),
        ] {
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: tid,
                    owner_id: owner_cid,
                    location_id: loc_ship,
                    flag: InventoryFlag::Cargo,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        }
        for (idx, ship_id) in [810_002i64, 810_003, 810_004].into_iter().enumerate() {
            snap.items.insert(
                825_000 + idx as i64,
                InventoryItemRecord {
                    item_id: 825_000 + idx as i64,
                    type_id: 16274,
                    owner_id: owner_cid + 1 + idx as i64,
                    location_id: ship_id,
                    flag: InventoryFlag::Cargo,
                    quantity: 20_000,
                    singleton: false,
                    version: 1,
                    ..Default::default()
                },
            );
        }
    });

    // 1. Skill check: Anchoring III (11584 lvl 3) required for 57319
    assert_eq!(
        rpc(&ctx, &mut owner_sess, "posMgr", "AnchorStructure", &[PyRep::Integer(std_beacon_id)])
            .await
            .unwrap_err()
            .code,
        "SkillMissing"
    );
    grant_skills(&ctx, owner_cid, &[(11584, 3)]);

    // 2. Fleet requirement: deploying without fleet fails with MustBeInFleet
    assert_eq!(
        rpc(&ctx, &mut owner_sess, "posMgr", "AnchorStructure", &[PyRep::Integer(std_beacon_id)])
            .await
            .unwrap_err()
            .code,
        "MustBeInFleet"
    );

    let fleet_id = 77_001i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.fleets.insert(
            fleet_id,
            eve_db::FleetRecord {
                fleet_id,
                boss_id: owner_cid,
                members: vec![owner_cid, fleet_cap_cid, fleet_blops_cid],
                ..Default::default()
            },
        );
    });
    owner_sess.fleet_id = Some(fleet_id);
    cap_sess.fleet_id = Some(fleet_id);
    blops_sess.fleet_id = Some(fleet_id);

    // 3. High-sec check: move owner briefly to Jita -> CannotLightCynoInSystem
    owner_sess.solar_system_id = Some(HIGHSEC_JITA);
    ctx.db.with_mut_snapshot(|s| s.characters.get_mut(&owner_cid).unwrap().solar_system_id = HIGHSEC_JITA);
    let _ = ctx
        .systems
        .get_or_spawn(HIGHSEC_JITA)
        .spawn_ball(Ball {
            id: 810_001,
            type_id: 626,
            position: deep_pos,
            velocity: DVec3::ZERO,
            max_velocity: 200.0,
            inertia: 3.0,
            mass: 10_000_000.0,
            radius: 100.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    assert_eq!(
        rpc(&ctx, &mut owner_sess, "posMgr", "AnchorStructure", &[PyRep::Integer(hs_beacon_id)])
            .await
            .unwrap_err()
            .code,
        "CannotLightCynoInSystem"
    );
    owner_sess.solar_system_id = Some(LOWSEC_VARD);
    ctx.db.with_mut_snapshot(|s| s.characters.get_mut(&owner_cid).unwrap().solar_system_id = LOWSEC_VARD);

    // 4. Deploy 57319 in low-sec Vard; verify single-use unanchor rejection
    let anchor_rep = rpc(&ctx, &mut owner_sess, "posMgr", "AnchorStructure", &[PyRep::Integer(std_beacon_id)])
        .await
        .expect("Anchor Mobile Cynosural Beacon");
    assert_eq!(kv_get(&anchor_rep, "anchoringDelaySeconds"), Some(&PyRep::Real(60.0)));
    assert_eq!(
        kv_get(&anchor_rep, "despawnUnix"),
        Some(&PyRep::Integer((ctx.now_unix() + 3_660) as i64))
    );
    assert_eq!(
        rpc(&ctx, &mut owner_sess, "entity", "CmdUnanchor", &[PyRep::Integer(std_beacon_id)])
            .await
            .unwrap_err()
            .code,
        "CannotUnanchor"
    );

    // 5. During 60s spool: map.GetBeaconCount excludes it AND fleet jump returns NoActiveBeacon
    ctx.advance_secs(30);
    let PyRep::Dict(pre_cnt) = rpc(&ctx, &mut owner_sess, "map", "GetBeaconCount", &[]).await.unwrap() else {
        panic!("expected dict");
    };
    assert!(!pre_cnt.iter().any(|(k, _)| k.as_i64() == Some(LOWSEC_VARD as i64)));
    assert_eq!(
        rpc(
            &ctx,
            &mut cap_sess,
            "beyonce",
            "CmdJumpToFleetDeployableBeacon",
            &[PyRep::Integer(owner_cid), PyRep::Integer(LOWSEC_VARD as i64), PyRep::Integer(std_beacon_id)],
        )
        .await
        .unwrap_err()
        .code,
        "NoActiveBeacon"
    );

    // 6. After 60s spool: map.GetBeaconCount includes Vard (+1), non-fleet corp mate is rejected, fleet Archon jumps!
    ctx.advance_secs(31);
    let post_counts = rpc(&ctx, &mut owner_sess, "map", "GetBeaconCount", &[]).await.unwrap();
    let PyRep::Dict(cnt_entries) = post_counts else { panic!("expected dict") };
    assert!(cnt_entries.iter().any(|(k, v)| {
        k.as_i64() == Some(LOWSEC_VARD as i64)
            && (v.as_i64() == Some(1)
                || matches!(v, PyRep::Tuple(t) if t.first().and_then(|x| x.as_i64()) == Some(1)))
    }));

    assert_eq!(
        rpc(
            &ctx,
            &mut nonfleet_sess,
            "beyonce",
            "CmdJumpToFleetDeployableBeacon",
            &[PyRep::Integer(owner_cid), PyRep::Integer(LOWSEC_VARD as i64), PyRep::Integer(std_beacon_id)],
        )
        .await
        .unwrap_err()
        .code,
        "NoActiveBeacon",
        "Non-fleet corp mate must NOT see or jump to fleet-only Mobile Cynosural Beacon"
    );

    let jump_res = rpc(
        &ctx,
        &mut cap_sess,
        "beyonce",
        "CmdJumpToFleetDeployableBeacon",
        &[PyRep::Integer(owner_cid), PyRep::Integer(LOWSEC_VARD as i64), PyRep::Integer(std_beacon_id)],
    )
    .await
    .expect("Fleet Archon jumps to active Mobile Cynosural Beacon");
    assert_eq!(kv_get(&jump_res, "solarSystemID"), Some(&PyRep::Integer(LOWSEC_VARD as i64)));

    // 7. Covert Mobile Cynosural Beacon (59630): requires Anchoring V + Cynosural Field Theory V (21603 lvl 5),
    //    bypasses Mobile Cynosural Inhibitor (33476), stays hidden from map.GetBeaconCount, rejects Archon, allows Redeemer!
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            820_099,
            InventoryItemRecord {
                item_id: 820_099,
                type_id: 33476,
                owner_id: nonfleet_corp_cid,
                location_id: LOWSEC_VARD as i64,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.item_positions.insert(820_099, (deep_pos + DVec3::new(10_000.0, 0.0, 0.0)).to_array());
    });
    assert_eq!(
        rpc(&ctx, &mut owner_sess, "posMgr", "AnchorStructure", &[PyRep::Integer(cov_beacon_id)])
            .await
            .unwrap_err()
            .code,
        "SkillMissing"
    );
    grant_skills(&ctx, owner_cid, &[(11584, 5), (21603, 5)]);
    let cov_rep = rpc(&ctx, &mut owner_sess, "posMgr", "AnchorStructure", &[PyRep::Integer(cov_beacon_id)])
        .await
        .expect("Covert Mobile Cyno Beacon bypasses Mobile Cyno Inhibitor");
    assert_eq!(
        kv_get(&cov_rep, "despawnUnix"),
        Some(&PyRep::Integer((ctx.now_unix() + 1_860) as i64))
    );
    ctx.advance_secs(65);

    let cov_counts = rpc(&ctx, &mut owner_sess, "map", "GetBeaconCount", &[]).await.unwrap();
    let PyRep::Dict(cov_cnt_entries) = cov_counts else { panic!("expected dict") };
    assert!(!cov_cnt_entries.iter().any(|(k, _)| k.as_i64() == Some(LOWSEC_VARD as i64)));

    let mut cap_sess2 =
        spawn_pilot_in_space(&ctx, 705, fleet_cap_cid, corp_id, 810_002, 23757, LOWSEC_AMAMAKE, deep_pos).await;
    cap_sess2.fleet_id = Some(fleet_id);
    assert_eq!(
        rpc(
            &ctx,
            &mut cap_sess2,
            "beyonce",
            "CmdJumpToFleetDeployableBeacon",
            &[PyRep::Integer(owner_cid), PyRep::Integer(LOWSEC_VARD as i64), PyRep::Integer(cov_beacon_id)],
        )
        .await
        .unwrap_err()
        .code,
        "ShipCannotJumpToBeacon"
    );

    let blops_jump = rpc(
        &ctx,
        &mut blops_sess,
        "beyonce",
        "CmdJumpToFleetDeployableBeacon",
        &[PyRep::Integer(owner_cid), PyRep::Integer(LOWSEC_VARD as i64), PyRep::Integer(cov_beacon_id)],
    )
    .await
    .expect("Black Ops Redeemer jumps to Covert Mobile Cynosural Beacon");
    assert_eq!(kv_get(&blops_jump, "solarSystemID"), Some(&PyRep::Integer(LOWSEC_VARD as i64)));

    ctx.advance_secs(1_810);
    let _ = rpc(&ctx, &mut owner_sess, "beyonce", "UpdateStateRequest", &[]).await;
    assert!(ctx.db.with_snapshot(|s| !s.items.contains_key(&cov_beacon_id)));
}

#[tokio::test]
async fn test_mobile_micro_jump_unit_component_spool_mass_scram_and_concurrent_use() {
    // parity: feat:structures.mobile-micro-jump-unit
    eve_testkit::attest(&["feat:structures.mobile-micro-jump-unit"]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let base_pos = DVec3::new(90_000_000.0, 90_000_000.0, 90_000_000.0);

    let owner_cid = 90_000_211;
    let hostile_cid = 90_000_212;
    let scram_cid = 90_000_213;
    let heavy_cid = 90_000_214;
    let far_cid = 90_000_215;

    let mut owner_sess =
        spawn_pilot_in_space(&ctx, 711, owner_cid, 98_002_211, 830_001, 626, LOWSEC_AMAMAKE, base_pos).await;
    let mut hostile_sess = spawn_pilot_in_space(
        &ctx,
        712,
        hostile_cid,
        98_002_212,
        830_002,
        626,
        LOWSEC_AMAMAKE,
        base_pos + DVec3::new(1_000.0, 500.0, 0.0),
    )
    .await;
    let mut scram_sess = spawn_pilot_in_space(
        &ctx,
        713,
        scram_cid,
        98_002_213,
        830_003,
        626,
        LOWSEC_AMAMAKE,
        base_pos + DVec3::new(800.0, -500.0, 0.0),
    )
    .await;
    let mut heavy_sess = spawn_pilot_in_space(
        &ctx,
        714,
        heavy_cid,
        98_002_214,
        830_004,
        23757,
        LOWSEC_AMAMAKE,
        base_pos + DVec3::new(1_500.0, 0.0, 0.0),
    )
    .await;
    let mut far_sess = spawn_pilot_in_space(
        &ctx,
        715,
        far_cid,
        98_002_215,
        830_005,
        626,
        LOWSEC_AMAMAKE,
        base_pos + DVec3::new(12_000.0, 0.0, 0.0),
    )
    .await;

    grant_skills(&ctx, owner_cid, &[(11584, 1)]);
    let mmju_id = 835_001i64;
    let mmju_too_close_id = 835_002i64;
    ctx.db.with_mut_snapshot(|snap| {
        for iid in [mmju_id, mmju_too_close_id] {
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: 33591,
                    owner_id: owner_cid,
                    location_id: 830_001,
                    flag: InventoryFlag::Cargo,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        }
    });

    rpc(&ctx, &mut owner_sess, "posMgr", "AnchorStructure", &[PyRep::Integer(mmju_id)])
        .await
        .expect("Anchor MMJU");

    assert_eq!(
        rpc(&ctx, &mut owner_sess, "posMgr", "AnchorStructure", &[PyRep::Integer(mmju_too_close_id)])
            .await
            .unwrap_err()
            .code,
        "DeploymentTooCloseToSameType"
    );
    assert_eq!(
        rpc(&ctx, &mut owner_sess, "entity", "CmdUnanchor", &[PyRep::Integer(mmju_id)])
            .await
            .unwrap_err()
            .code,
        "CannotUnanchor"
    );

    let comp_args = [
        PyRep::Integer(mmju_id),
        PyRep::String("microJumpDriver".into()),
        PyRep::String("StartMicroJumpDriveForShip".into()),
    ];
    assert_eq!(
        rpc(&ctx, &mut owner_sess, "beyonce", "CallComponentFromClient", &comp_args)
            .await
            .unwrap_err()
            .code,
        "DeployableAnchoring"
    );

    ctx.advance_secs(61);

    assert_eq!(
        rpc(&ctx, &mut far_sess, "beyonce", "CallComponentFromClient", &comp_args)
            .await
            .unwrap_err()
            .code,
        "OutOfRange"
    );
    assert_eq!(
        rpc(&ctx, &mut heavy_sess, "beyonce", "CallComponentFromClient", &comp_args)
            .await
            .unwrap_err()
            .code,
        "ShipTooHeavy"
    );

    let sys_h = ctx.systems.get_or_spawn(LOWSEC_AMAMAKE);
    let owner_pre = sys_h.get_ball_snapshot(830_001).await.unwrap().unwrap().1.position;
    let hostile_pre = sys_h.get_ball_snapshot(830_002).await.unwrap().unwrap().1.position;
    let scram_pre = sys_h.get_ball_snapshot(830_003).await.unwrap().unwrap().1.position;

    for s in [&mut owner_sess, &mut hostile_sess, &mut scram_sess] {
        let rep = rpc(&ctx, s, "beyonce", "CallComponentFromClient", &comp_args)
            .await
            .expect("Concurrent MMJU StartMicroJumpDriveForShip");
        assert_eq!(kv_get(&rep, "spoolSeconds"), Some(&PyRep::Real(12.0)));
    }

    ctx.advance_secs(5);
    assert!((sys_h.get_ball_snapshot(830_001).await.unwrap().unwrap().1.position - owner_pre).length() < 1_000.0);
    sys_h
        .add_interdiction_sphere(999_830, scram_pre, 500.0)
        .await
        .unwrap();
    assert_eq!(
        rpc(&ctx, &mut scram_sess, "beyonce", "CallComponentFromClient", &comp_args)
            .await
            .unwrap_err()
            .code,
        "WarpScrambled"
    );

    ctx.advance_secs(8);
    let _ = rpc(&ctx, &mut owner_sess, "beyonce", "UpdateStateRequest", &[]).await;

    let owner_post = sys_h.get_ball_snapshot(830_001).await.unwrap().unwrap().1.position;
    let hostile_post = sys_h.get_ball_snapshot(830_002).await.unwrap().unwrap().1.position;
    let scram_post = sys_h.get_ball_snapshot(830_003).await.unwrap().unwrap().1.position;
    assert!(
        ((owner_post - owner_pre).length() - 100_000.0).abs() < 500.0,
        "Owner cruiser must translate 100 km (got {:.1} m)",
        (owner_post - owner_pre).length()
    );
    assert!(
        ((hostile_post - hostile_pre).length() - 100_000.0).abs() < 500.0,
        "Hostile cruiser using public MMJU must also translate 100 km (got {:.1} m)",
        (hostile_post - hostile_pre).length()
    );
    assert!(
        (scram_post - scram_pre).length() < 500.0,
        "Mid-spool scrambled cruiser must NOT jump (moved {:.1} m)",
        (scram_post - scram_pre).length()
    );

    // Mid-spool cloak and mid-spool unit expiry must both prevent jump on completion
    let _ = sys_h.set_ball_position(830_001, owner_pre).await;
    let _ = sys_h.set_ball_position(830_002, hostile_pre).await;
    for s in [&mut owner_sess, &mut hostile_sess] {
        rpc(&ctx, s, "beyonce", "CallComponentFromClient", &comp_args)
            .await
            .expect("Start second MMJU spool");
    }
    if let Ok(mut bp) = sys_h.ballpark.write() {
        if let Some(cs) = bp.combat_states.get_mut(&830_001) {
            cs.is_cloaked = true;
        }
    }
    ctx.db.with_mut_snapshot(|snap| {
        eve_server::services::mobile_deployables::write_deployable_state_snap(snap, |st| {
            if let Some(rec) = st.deployables.get_mut(&mmju_id) {
                rec.expires_at_unix = ctx.now_unix() + 5;
            }
        });
    });
    ctx.advance_secs(13);
    let _ = rpc(&ctx, &mut owner_sess, "beyonce", "UpdateStateRequest", &[]).await;
    let owner_cloaked_pos = sys_h.get_ball_snapshot(830_001).await.unwrap().unwrap().1.position;
    let hostile_expired_pos = sys_h.get_ball_snapshot(830_002).await.unwrap().unwrap().1.position;
    assert!((owner_cloaked_pos - owner_pre).length() < 500.0, "Mid-spool cloaked ship must NOT jump");
    assert!((hostile_expired_pos - hostile_pre).length() < 500.0, "Ship on expired MMJU must NOT jump");
}

}

#[allow(dead_code, unused_imports)]
pub(crate) mod mobile_structures_retrieval {
    use super::*;

//  End-to-end V24 RPC integration tests for Step 4B Mobile Deployable Structures (Part 5):
//  - `feat:structures.mobile-observatory` (`58904`)
//  - `feat:structures.mobile-observatory-decloak-ping` (`58904` 10m pings, 40% roll, 15m cloak stabilization, `OnCloakModuleForceDeactivated`)
//  - `feat:structures.mobile-scan-inhibitor` (`33589` 30 km d-scan & combat probe masking)
//  - `feat:structures.mobile-siphon` (`33477` on-grid moon/reaction material siphoning + 10% SDE waste)


use super::mobile_structures_deployment::{
    grant_skills, kv_get, rpc, rpc_bound, spawn_pilot_in_space, HIGHSEC_JITA, LOWSEC_AMAMAKE,
};
use eve_db::{InventoryFlag, InventoryItemRecord, UpwellStructureRecord};
use eve_destiny::{Ball, MovementMode};
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use glam::DVec3;

const WH_SYSTEM: u32 = 31_000_007;
const POCHVEN_NALVULA: u32 = 30_000_192;
const ZARZAKH_SYS: u32 = 30_100_000;

#[tokio::test]
async fn test_mobile_observatory_restrictions_stabilization_and_decloak_pings() {
    // parity: feat:structures.mobile-observatory, feat:structures.mobile-observatory-decloak-ping
    eve_testkit::attest(&[
        "feat:structures.mobile-observatory",
        "feat:structures.mobile-observatory-decloak-ping",
    ]);

    let ctx = ServerContext::new().with_seeded_rng(42);
    crate::common::pin_clock(&ctx);
    let deep_pos = DVec3::new(75_000_000.0, 75_000_000.0, 75_000_000.0);

    let hunter_cid = 90_000_221;
    let stale_cloak_cid = 90_000_222;
    let fresh_cloak_cid = 90_000_223;

    let mut hunter_sess =
        spawn_pilot_in_space(&ctx, 721, hunter_cid, 98_002_221, 840_001, 626, LOWSEC_AMAMAKE, deep_pos).await;
    let mut stale_sess = spawn_pilot_in_space(
        &ctx,
        722,
        stale_cloak_cid,
        98_002_222,
        840_002,
        626,
        LOWSEC_AMAMAKE,
        deep_pos + DVec3::new(500_000.0, 0.0, 0.0),
    )
    .await;
    let mut fresh_sess = spawn_pilot_in_space(
        &ctx,
        723,
        fresh_cloak_cid,
        98_002_223,
        840_003,
        626,
        LOWSEC_AMAMAKE,
        deep_pos + DVec3::new(-500_000.0, 0.0, 0.0),
    )
    .await;

    grant_skills(&ctx, hunter_cid, &[(11584, 3)]);
    grant_skills(&ctx, stale_cloak_cid, &[(11579, 4)]);
    grant_skills(&ctx, fresh_cloak_cid, &[(11579, 4)]);

    ctx.db.with_mut_snapshot(|snap| {
        for tid_sys in [845_001i64, 845_002, 845_003, 845_004, 845_005] {
            snap.items.insert(
                tid_sys,
                InventoryItemRecord {
                    item_id: tid_sys,
                    type_id: 58904,
                    owner_id: hunter_cid,
                    location_id: 840_001,
                    flag: InventoryFlag::Cargo,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        }
        for (mod_id, ship_id, owner) in [(846_002i64, 840_002i64, stale_cloak_cid), (846_003, 840_003, fresh_cloak_cid)]
        {
            snap.items.insert(
                mod_id,
                InventoryItemRecord {
                    item_id: mod_id,
                    type_id: 11370,
                    owner_id: owner,
                    location_id: ship_id,
                    flag: InventoryFlag::HiSlot0,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
            snap.module_online_states.insert(mod_id, true);
        }
    });
    ctx.invalidate_ship_fitting(840_002);
    ctx.invalidate_ship_fitting(840_003);

    // 1. Verify space-class rejections: high-sec Jita, Wormhole, Pochven, Zarzakh
    for (bad_sys, item_id, expected_code) in [
        (HIGHSEC_JITA, 845_002i64, "CannotDeployInHighSec"),
        (WH_SYSTEM, 845_003i64, "CannotDeployInWormhole"),
        (POCHVEN_NALVULA, 845_004i64, "CannotDeployInPochven"),
        (ZARZAKH_SYS, 845_005i64, "AnchoringDisallowedInZarzakh"),
    ] {
        hunter_sess.solar_system_id = Some(bad_sys);
        ctx.db.with_mut_snapshot(|s| s.characters.get_mut(&hunter_cid).unwrap().solar_system_id = bad_sys);
        let _ = ctx
            .systems
            .get_or_spawn(bad_sys)
            .spawn_ball(Ball {
                id: 840_001,
                type_id: 626,
                position: deep_pos,
                velocity: DVec3::ZERO,
                max_velocity: 200.0,
                inertia: 3.0,
                mass: 10_000_000.0,
                radius: 100.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            })
            .await;
        assert_eq!(
            rpc(&ctx, &mut hunter_sess, "posMgr", "AnchorStructure", &[PyRep::Integer(item_id)])
                .await
                .unwrap_err()
                .code,
            expected_code
        );
    }
    hunter_sess.solar_system_id = Some(LOWSEC_AMAMAKE);
    ctx.db
        .with_mut_snapshot(|s| s.characters.get_mut(&hunter_cid).unwrap().solar_system_id = LOWSEC_AMAMAKE);

    // 2. Cloak `stale_cloak_cid` NOW (t = T0), then advance 400s before deploying Observatory!
    //    At ping #1 (`T0 + 400 + 600 + 600 = T0 + 1600`), `stale` has been cloaked 1,600s (>= 900s stabilization expired).
    rpc(&ctx, &mut stale_sess, "dogmaIM", "Activate", &[PyRep::Integer(846_002)])
        .await
        .expect("Activate cloak on stale ship");
    let sys_h = ctx.systems.get_or_spawn(LOWSEC_AMAMAKE);
    assert!(sys_h.get_combat_state(840_002).await.unwrap().unwrap().is_cloaked);

    ctx.advance_secs(400);
    let deploy_t = ctx.now_unix();
    let obs_rep = rpc(&ctx, &mut hunter_sess, "posMgr", "AnchorStructure", &[PyRep::Integer(845_001)])
        .await
        .expect("Anchor Mobile Observatory in low-sec");
    assert_eq!(kv_get(&obs_rep, "anchoringDelaySeconds"), Some(&PyRep::Real(600.0)));
    assert_eq!(
        kv_get(&obs_rep, "despawnUnix"),
        Some(&PyRep::Integer((deploy_t + 7_800) as i64))
    );

    // 3. Advance 800s (to deploy_t + 800): Observatory finished 600s deploy at deploy_t + 600, ping #1 is at deploy_t + 1200.
    //    Cloak `fresh_cloak_cid` NOW (deploy_t + 800) -> at ping #1 (deploy_t + 1200), only 400s (< 900s) has elapsed -> IMMUNE!
    ctx.advance_secs(800);
    rpc(&ctx, &mut fresh_sess, "dogmaIM", "Activate", &[PyRep::Integer(846_003)])
        .await
        .expect("Activate cloak on fresh stabilized ship");
    assert!(sys_h.get_combat_state(840_003).await.unwrap().unwrap().is_cloaked);

    let hit_seed = (1u64..200)
        .find(|&sd| eve_server::rng::MechanicRng::seeded(sd).roll(0.40))
        .expect("seed with < 0.40 first draw");
    ctx.set_mechanic_rng_seed(Some(hit_seed));

    ctx.advance_secs(405);
    let _ = rpc(&ctx, &mut hunter_sess, "beyonce", "UpdateStateRequest", &[]).await;

    assert!(
        !sys_h.get_combat_state(840_002).await.unwrap().unwrap().is_cloaked,
        "Stale cloaked ship (> 15m) must be decloaked on ping #1 when 40% roll hits"
    );
    assert!(
        sys_h.get_combat_state(840_003).await.unwrap().unwrap().is_cloaked,
        "Freshly cloaked ship (< 15m Cloak Stabilization) MUST be immune to Observatory ping #1"
    );
    let stale_notifs = ctx.drain_notifications_for_char(stale_cloak_cid);
    let stale_decloak = stale_notifs.iter().find(|n| n.event == "OnCloakModuleForceDeactivated").expect("Decloaked pilot must receive OnCloakModuleForceDeactivated");
    assert!(stale_decloak.args.len() == 2 && matches!(stale_decloak.args[0], PyRep::String(_)) && matches!(stale_decloak.args[1], PyRep::Dict(_)), "expected [notifyMsg, notifyArgs]");

    // 4. Advance across remaining 11 pings to `deploy_t + 7_805` -> `fresh` stabilization expires at `deploy_t + 1700`,
    //    subsequent pings decloak `fresh`, and the Observatory expires & despawns after 12 pings (2h active)!
    ctx.advance_secs(6_600);
    let _ = rpc(&ctx, &mut hunter_sess, "beyonce", "UpdateStateRequest", &[]).await;

    assert!(
        !sys_h.get_combat_state(840_003).await.unwrap().unwrap().is_cloaked,
        "Once 15m Cloak Stabilization expires, subsequent Observatory pings decloak fresh ship"
    );
    let fresh_notifs = ctx.drain_notifications_for_char(fresh_cloak_cid);
    let fresh_decloak = fresh_notifs.iter().find(|n| n.event == "OnCloakModuleForceDeactivated").expect("Fresh ship pilot must receive OnCloakModuleForceDeactivated once stabilization expires");
    assert!(fresh_decloak.args.len() == 2 && matches!(fresh_decloak.args[0], PyRep::String(_)) && matches!(fresh_decloak.args[1], PyRep::Dict(_)));
    assert!(
        ctx.db.with_snapshot(|s| !s.items.contains_key(&845_001)),
        "Mobile Observatory must automatically expire and despawn after 10m deploy + 2h (12 pings)"
    );
}

#[tokio::test]
async fn test_mobile_scan_inhibitor_and_mobile_siphon_unit_end_to_end() {
    // parity: feat:structures.mobile-scan-inhibitor, feat:structures.mobile-siphon
    eve_testkit::attest(&[
        "feat:structures.mobile-scan-inhibitor",
        "feat:structures.mobile-siphon",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let grid_origin = DVec3::new(65_000_000.0, 65_000_000.0, 65_000_000.0);

    let owner_cid = 90_000_231;
    let hidden_cid = 90_000_232;
    let exposed_cid = 90_000_233;
    let scanner_cid = 90_000_234;

    let mut owner_sess =
        spawn_pilot_in_space(&ctx, 731, owner_cid, 98_002_231, 850_001, 626, LOWSEC_AMAMAKE, grid_origin).await;
    let mut hidden_sess = spawn_pilot_in_space(
        &ctx,
        732,
        hidden_cid,
        98_002_232,
        850_002,
        626,
        LOWSEC_AMAMAKE,
        grid_origin + DVec3::new(15_000.0, 0.0, 0.0),
    )
    .await;
    let _exposed_sess = spawn_pilot_in_space(
        &ctx,
        733,
        exposed_cid,
        98_002_233,
        850_003,
        626,
        LOWSEC_AMAMAKE,
        grid_origin + DVec3::new(45_000.0, 0.0, 0.0),
    )
    .await;
    let mut scanner_sess = spawn_pilot_in_space(
        &ctx,
        734,
        scanner_cid,
        98_002_234,
        850_004,
        626,
        LOWSEC_AMAMAKE,
        grid_origin + DVec3::new(80_000.0, 0.0, 0.0),
    )
    .await;

    let sys_h = ctx.systems.get_or_spawn(LOWSEC_AMAMAKE);
    for sid in [850_001i64, 850_002, 850_003, 850_004] {
        let _ = sys_h.set_movement_mode(sid, MovementMode::Stop).await;
    }

    grant_skills(&ctx, owner_cid, &[(11584, 3)]);
    grant_skills(&ctx, scanner_cid, &[(3412, 5), (25810, 5), (25811, 5)]);

    let inhib_id = 855_001i64;
    let inhib_dup_id = 855_002i64;
    ctx.db.with_mut_snapshot(|snap| {
        for iid in [inhib_id, inhib_dup_id] {
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: 33589,
                    owner_id: owner_cid,
                    location_id: 850_001,
                    flag: InventoryFlag::Cargo,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        }
        snap.items.insert(
            856_001,
            InventoryItemRecord {
                item_id: 856_001,
                type_id: 18639,
                owner_id: scanner_cid,
                location_id: 850_004,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            856_002,
            InventoryItemRecord {
                item_id: 856_002,
                type_id: 30028,
                owner_id: scanner_cid,
                location_id: 850_004,
                flag: InventoryFlag::HiSlot0,
                quantity: 8,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });
    ctx.invalidate_ship_fitting(850_004);

    // 1. Anchor Mobile Scan Inhibitor (33589); verify < 100 km same-type block and pre-spool d-scan visibility
    rpc(&ctx, &mut owner_sess, "posMgr", "AnchorStructure", &[PyRep::Integer(inhib_id)])
        .await
        .expect("Anchor Mobile Scan Inhibitor");
    assert_eq!(
        rpc(&ctx, &mut owner_sess, "posMgr", "AnchorStructure", &[PyRep::Integer(inhib_dup_id)])
            .await
            .unwrap_err()
            .code,
        "DeploymentTooCloseToSameType"
    );

    let pre_dscan = rpc(
        &ctx,
        &mut scanner_sess,
        "scanMgr",
        "ConeScan",
        &[PyRep::Real(360.0), PyRep::Real(1_000_000_000.0), PyRep::Real(0.0), PyRep::Real(0.0), PyRep::Real(1.0)],
    )
    .await
    .unwrap();
    let PyRep::List(pre_rows) = pre_dscan else { panic!("expected list") };
    assert!(
        pre_rows.iter().any(|r| kv_get(r, "itemID") == Some(&PyRep::Integer(850_002))),
        "Before 60s spool completes, ship at 15 km is still visible on ConeScan"
    );

    // 2. Complete 60s spool: hidden_ship (15 km) is masked on BOTH ConeScan AND Combat Probes,
    //    while exposed_ship (45 km) AND the Mobile Scan Inhibitor (33589) itself remain visible!
    ctx.advance_secs(61);
    let post_dscan = rpc(
        &ctx,
        &mut scanner_sess,
        "scanMgr",
        "ConeScan",
        &[PyRep::Real(360.0), PyRep::Real(1_000_000_000.0), PyRep::Real(0.0), PyRep::Real(0.0), PyRep::Real(1.0)],
    )
    .await
    .unwrap();
    let PyRep::List(post_rows) = post_dscan else { panic!("expected list") };
    assert!(!post_rows.iter().any(|r| kv_get(r, "itemID") == Some(&PyRep::Integer(850_002))), "Ship at 15 km must be hidden from ConeScan");
    assert!(post_rows.iter().any(|r| kv_get(r, "itemID") == Some(&PyRep::Integer(850_003))), "Ship at 45 km must remain visible on ConeScan");
    assert!(post_rows.iter().any(|r| kv_get(r, "itemID") == Some(&PyRep::Integer(inhib_id))), "Mobile Scan Inhibitor itself must remain visible on ConeScan");

    let blinded_dscan = rpc(
        &ctx,
        &mut hidden_sess,
        "scanMgr",
        "ConeScan",
        &[PyRep::Real(360.0), PyRep::Real(1_000_000_000.0), PyRep::Real(0.0), PyRep::Real(0.0), PyRep::Real(1.0)],
    )
    .await
    .unwrap();
    assert_eq!(blinded_dscan, PyRep::List(Vec::new()));

    rpc(
        &ctx,
        &mut scanner_sess,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(856_001), PyRep::String("launchScanners".into()), PyRep::None, PyRep::Integer(0)],
    )
    .await
    .expect("Launch Combat Scanner Probes");
    let probe_ids: Vec<i64> = ctx
        .active_scanner_probes
        .read()
        .unwrap()
        .values()
        .filter(|p| p.owner_char_id == scanner_cid)
        .map(|p| p.probe_id)
        .collect();
    let probe_dict = PyRep::Dict(
        probe_ids
            .iter()
            .map(|&pid| {
                (
                    PyRep::Integer(pid),
                    PyRep::keyval([
                        ("pos", PyRep::Tuple(vec![PyRep::Real(grid_origin.x), PyRep::Real(grid_origin.y), PyRep::Real(grid_origin.z)])),
                        ("scanRange", PyRep::Real(150_000_000_000.0)),
                    ]),
                )
            })
            .collect(),
    );
    let scan_bound = rpc(&ctx, &mut scanner_sess, "scanMgr", "GetSystemScanMgr", &[])
        .await
        .unwrap()
        .dict_get("boundID")
        .and_then(|v| v.as_i64())
        .map(|v| v as u64);
    let PyRep::List(cmb_rows) = rpc_bound(&ctx, &mut scanner_sess, "scanMgr", scan_bound, "RequestScans", &[probe_dict])
        .await
        .unwrap()
    else {
        panic!("expected RequestScans list")
    };
    assert!(!cmb_rows.iter().any(|r| kv_get(r, "itemID") == Some(&PyRep::Integer(850_002))), "Ship at 15 km must be hidden from Combat Probes");
    assert!(cmb_rows.iter().any(|r| kv_get(r, "itemID") == Some(&PyRep::Integer(850_003))), "Ship at 45 km must be detected by Combat Probes");
    assert!(cmb_rows.iter().any(|r| kv_get(r, "itemID") == Some(&PyRep::Integer(inhib_id))), "Mobile Scan Inhibitor itself must be detected by Combat Probes");

    // 3. Mobile Siphon Unit (33477): off-grid rejection, on-grid Athanor siphoning + 10% SDE waste, public looting, empty unanchor & scoop!
    let siphon_pos = DVec3::new(-65_000_000.0, 65_000_000.0, 65_000_000.0);
    let mut siphon_sess =
        spawn_pilot_in_space(&ctx, 735, owner_cid, 98_002_231, 850_011, 626, LOWSEC_AMAMAKE, siphon_pos).await;
    let mut looter_sess = spawn_pilot_in_space(
        &ctx,
        736,
        scanner_cid,
        98_002_234,
        850_012,
        626,
        LOWSEC_AMAMAKE,
        siphon_pos + DVec3::new(1_000.0, 0.0, 0.0),
    )
    .await;
    for sid in [850_011i64, 850_012] {
        let _ = sys_h.set_movement_mode(sid, MovementMode::Stop).await;
    }
    let siphon_id = 857_001i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            siphon_id,
            InventoryItemRecord {
                item_id: siphon_id,
                type_id: 33477,
                owner_id: owner_cid,
                location_id: 850_011,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });

    assert_eq!(
        rpc(&ctx, &mut siphon_sess, "posMgr", "AnchorStructure", &[PyRep::Integer(siphon_id)])
            .await
            .unwrap_err()
            .code,
        "NoSiphonTargetInRange"
    );

    let athanor_id = 1_000_000_857_000i64;
    let athanor_pos = siphon_pos + DVec3::new(60_000.0, 0.0, 0.0);
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            athanor_id,
            UpwellStructureRecord {
                structure_id: athanor_id,
                type_id: 35835,
                owner_corp_id: 98_002_999,
                solar_system_id: LOWSEC_AMAMAKE,
                position: athanor_pos.to_array(),
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 1_200,
                shield_hp: 1_000_000.0,
                armor_hp: 1_000_000.0,
                hull_hp: 1_000_000.0,
                name: "Moon Refinery".into(),
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
        snap.item_positions.insert(athanor_id, athanor_pos.to_array());
        snap.items.insert(
            858_010,
            InventoryItemRecord {
                item_id: 858_010,
                type_id: 16634,
                owner_id: owner_cid,
                location_id: athanor_id,
                flag: InventoryFlag::Hangar,
                quantity: 500,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            858_011,
            InventoryItemRecord {
                item_id: 858_011,
                type_id: 16670,
                owner_id: 98_002_999,
                location_id: athanor_id,
                flag: InventoryFlag::Hangar,
                quantity: 500,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    assert_eq!(
        rpc(&ctx, &mut siphon_sess, "posMgr", "AnchorStructure", &[PyRep::Integer(siphon_id)])
            .await
            .unwrap_err()
            .code,
        "NoSiphonTargetInRange",
        "Upwell Athanor with personal/corp hangar stock must not be a valid siphon target"
    );

    let silo_id = 1_000_000_857_100i64;
    let silo_pos = siphon_pos + DVec3::new(0.0, 60_000.0, 0.0);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            silo_id,
            InventoryItemRecord {
                item_id: silo_id,
                type_id: 14343,
                owner_id: 98_002_999,
                location_id: LOWSEC_AMAMAKE as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.item_positions.insert(silo_id, silo_pos.to_array());
        snap.items.insert(
            858_001,
            InventoryItemRecord {
                item_id: 858_001,
                type_id: 16634,
                owner_id: 98_002_999,
                location_id: silo_id,
                flag: InventoryFlag::Cargo,
                quantity: 500,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            858_002,
            InventoryItemRecord {
                item_id: 858_002,
                type_id: 16670,
                owner_id: 98_002_999,
                location_id: silo_id,
                flag: InventoryFlag::Cargo,
                quantity: 500,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    rpc(&ctx, &mut siphon_sess, "posMgr", "AnchorStructure", &[PyRep::Integer(siphon_id)])
        .await
        .expect("Anchor Mobile Siphon Unit within 150 km of Starbase Silo");

    ctx.advance_secs(7_265);
    let fill_ratio = rpc(&ctx, &mut looter_sess, "beyonce", "GetCapacityOfSiphon", &[PyRep::Integer(siphon_id)])
        .await
        .expect("GetCapacityOfSiphon")
        .as_f64()
        .unwrap();
    assert!(fill_ratio > 0.0, "Siphon fill ratio must be > 0 after 2 cycles");

    let (rem_raw, rem_pro, athanor_personal, athanor_corp, stolen_items) = ctx.db.with_snapshot(|s| {
        let r = s.items[&858_001].quantity;
        let p = s.items[&858_002].quantity;
        let ap = s.items[&858_010].quantity;
        let ac = s.items[&858_011].quantity;
        let st: Vec<(i64, u32, i32)> = s
            .items
            .values()
            .filter(|i| i.location_id == siphon_id)
            .map(|i| (i.item_id, i.type_id, i.quantity))
            .collect();
        (r, p, ap, ac, st)
    });
    assert_eq!(rem_raw, 368, "Starbase Silo must lose 120 stolen + 12 (10%) waste = 132 raw moon material");
    assert_eq!(rem_pro, 434, "Starbase Silo must lose 60 stolen + 6 (10%) waste = 66 processed material");
    assert_eq!(athanor_personal, 500, "Athanor personal hangar stock must not be siphoned");
    assert_eq!(athanor_corp, 500, "Athanor corp hangar stock must not be siphoned");
    assert!(stolen_items.iter().any(|&(_, tid, q)| tid == 16634 && q == 120));
    assert!(stolen_items.iter().any(|&(_, tid, q)| tid == 16670 && q == 60));

    assert_eq!(
        rpc(&ctx, &mut looter_sess, "invbroker", "ValidateItemCanBeOpened", &[PyRep::Integer(siphon_id)])
            .await
            .unwrap(),
        PyRep::Bool(true)
    );
    let PyRep::Tuple(bind_parts) = rpc(
        &ctx,
        &mut looter_sess,
        "invbroker",
        "MachoBindObject",
        &[PyRep::Tuple(vec![PyRep::Integer(LOWSEC_AMAMAKE as i64), PyRep::Integer(5)]), PyRep::None],
    )
    .await
    .expect("Bind solar-system invbroker") else {
        panic!("expected MachoBindObject tuple");
    };
    let sys_inv_mgr = bind_parts[0].as_i64().unwrap() as u64;
    let ship_inv_bound = Some(
        rpc_bound(
            &ctx,
            &mut looter_sess,
            "invbroker",
            Some(sys_inv_mgr),
            "GetInventoryFromId",
            &[PyRep::Integer(850_012), PyRep::Integer(0)],
        )
        .await
        .expect("GetInventoryFromId looter ship")
        .as_i64()
        .unwrap() as u64,
    );

    rpc(&ctx, &mut siphon_sess, "entity", "CmdUnanchor", &[PyRep::Integer(siphon_id)])
        .await
        .expect("Unanchor Mobile Siphon Unit");
    assert_eq!(
        rpc(&ctx, &mut siphon_sess, "ship", "Scoop", &[PyRep::Integer(siphon_id)])
            .await
            .unwrap_err()
            .code,
        "CannotPutContainerInContainer"
    );

    for (st_id, _, _qty) in &stolen_items {
        rpc_bound(
            &ctx,
            &mut looter_sess,
            "invbroker",
            ship_inv_bound,
            "Add",
            &[PyRep::Integer(*st_id), PyRep::Integer(siphon_id), PyRep::Integer(5)],
        )
        .await
        .expect("Third-party pilot loots stolen material from Mobile Siphon Unit");
    }
    rpc(&ctx, &mut siphon_sess, "ship", "Scoop", &[PyRep::Integer(siphon_id)])
        .await
        .expect("Owner scoops empty unanchored Mobile Siphon Unit");
    assert_eq!(ctx.db.with_snapshot(|s| s.items[&siphon_id].location_id), 850_011);
}

}
