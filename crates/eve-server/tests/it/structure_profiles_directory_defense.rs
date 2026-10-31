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

    // Fit online Standup Multirole Missile Launcher I (35922) to both Fortizars so auto-defense deals real volley damage
    ctx.db.with_mut_snapshot(|snap| {
        for (mod_id, sid) in [(910_001, low_st), (910_002, high_st)] {
            snap.items.insert(mod_id, InventoryItemRecord {
                item_id: mod_id, type_id: 35922, owner_id: corp_id, location_id: sid,
                flag: InventoryFlag::HiSlot0, quantity: 1, singleton: true, version: 1, ..Default::default()
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
        let _s = spawn_and_enter_grid(&ctx, 530 + idx as u64, cid, ship_id, lowsec_sys, DVec3::new(18_000.0, idx as f64 * 2_000.0, 0.0)).await;
        let cs = low_sys_h.get_combat_state(ship_id).await.unwrap().unwrap();
        assert!(cs.shield_hp < cs.max_shield_hp, "Hostile/red/blocked pilot {cid} in low-sec must take auto-defense volley damage");
        let notifs = ctx.drain_notifications_for_char(cid);
        assert!(notifs.iter().any(|n| n.event == "OnTarget"), "Target must receive OnTarget lock notification");
        assert!(notifs.iter().any(|n| n.event == "OnDamageMessage"), "Target must receive OnDamageMessage");
    }

    // 3. Neutral pilot enters grid unharmed until firing on the structure -> live combat retaliation!
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
