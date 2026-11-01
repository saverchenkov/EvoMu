//! Integration tests for open `feat:` parity rows (Batch 7, Part 2).
//!
//! Covers:
//! - Ship SKIN application in space with observer slim/cosmetic updates & SQLite persistence (`feat:cosmetics.skin-apply`)
//! - Structure directory listing accessible structures with online services & ACL gating (`feat:structures.structure-directory`)

#![forbid(unsafe_code)]

use eve_db::{InventoryFlag, InventoryItemRecord, UpwellStructureRecord};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use glam::DVec3;

use crate::open_feats_and_rpcs_b7::{rpc, setup_pilot, setup_pilot_for_user};

const PILOT_1: i64 = 90_000_001;
const PILOT_2: i64 = 90_000_002;
const STATION_ID: i64 = 60_003_760;
const JITA_SYS: u32 = 30_000_142;

#[tokio::test]
async fn test_cosmetics_skin_apply_in_space_broadcast_and_persistence() {
    // parity: feat:cosmetics.skin-apply
    let ctx = ServerContext::new();
    let mut sess1 = setup_pilot(&ctx, PILOT_1).await;
    let _sess2 = setup_pilot_for_user(&ctx, 77_002, "other", "other-secret", PILOT_2).await;

    let active_ship_id = ctx.db.with_mut_snapshot(|snap| {
        let ship_id = snap.characters[&PILOT_1].active_ship_id;
        if let Some(sh) = snap.items.get_mut(&ship_id) {
            sh.type_id = 587; // Rifter
        }
        ship_id
    });

    // Find a real SDE permanent SKIN license item for Rifter (type 587)
    let (license_type_id, resolved_skin_id) = ctx
        .sde
        .skin_licenses
        .iter()
        .find_map(|(&lic_tid, lic)| {
            let skin = ctx.sde.skins.get(&lic.skin_id)?;
            let ty = ctx.sde.types.get(&lic_tid)?;
            if ty.category_id == 91 && skin.types.contains(&587) {
                Some((lic_tid, lic.skin_id))
            } else {
                None
            }
        })
        .expect("SDE Rifter SKIN license");

    // Find a valid SDE SKIN for another hull (not Rifter) to test hull-mismatch rejection
    let wrong_hull_skin_id = ctx
        .sde
        .skins
        .values()
        .find(|sk| !sk.types.is_empty() && !sk.types.contains(&587))
        .expect("non-Rifter SDE skin")
        .skin_id;

    // Negative: unlicensed SKIN rejected before license consumption
    let unlic_err = rpc(
        &ctx,
        &mut sess1,
        "shipCosmeticsMgr",
        "ApplySkinToShip",
        &[
            PyRep::Integer(active_ship_id),
            PyRep::Integer(i64::from(resolved_skin_id)),
        ],
    )
    .await
    .expect_err("unlicensed SKIN must fail");
    assert_eq!(unlic_err.code, "SkinNotLicensed");

    // Negative: wrong hull SKIN rejected
    let hull_err = rpc(
        &ctx,
        &mut sess1,
        "shipCosmeticsMgr",
        "ApplySkinToShip",
        &[
            PyRep::Integer(active_ship_id),
            PyRep::Integer(i64::from(wrong_hull_skin_id)),
        ],
    )
    .await
    .expect_err("incompatible hull SKIN must fail");
    assert_eq!(hull_err.code, "InvalidShipTypeForSkin");

    // Seed real SKIN license item in hangar and consume via ActivateSkinLicense
    let lic_item_id = 990_701_001i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            lic_item_id,
            InventoryItemRecord {
                item_id: lic_item_id,
                type_id: license_type_id,
                owner_id: PILOT_1,
                location_id: STATION_ID,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: false,
                version: 1,
                dynamic_attributes: Default::default(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });
    rpc(
        &ctx,
        &mut sess1,
        "shipCosmeticsMgr",
        "ActivateSkinLicense",
        &[PyRep::Integer(i64::from(license_type_id))],
    )
    .await
    .expect("ActivateSkinLicense");
    assert!(
        ctx.db.with_snapshot(|s| !s.items.contains_key(&lic_item_id)),
        "consumed SKIN license item must be removed from hangar"
    );

    // Put both PILOT_1 (in active Rifter) and observer PILOT_2 into space in Jita
    ctx.db.with_mut_snapshot(|snap| {
        for cid in [PILOT_1, PILOT_2] {
            if let Some(ch) = snap.characters.get_mut(&cid) {
                ch.solar_system_id = JITA_SYS;
                ch.station_id = None;
                ch.structure_id = None;
            }
        }
    });
    sess1.station_id = None;
    let sys_h = ctx.systems.get_or_spawn(JITA_SYS);
    let _ = sys_h
        .spawn_ball(crate::sovereignty_equinox_gameplay_closure::make_ball(
            active_ship_id,
            587,
            DVec3::new(10_000.0, 0.0, 0.0),
        ))
        .await;

    let _ = ctx.drain_notifications_for_char(PILOT_1);
    let _ = ctx.drain_notifications_for_char(PILOT_2);

    // Apply licensed SKIN to active ship in space
    let apply_rep = rpc(
        &ctx,
        &mut sess1,
        "shipCosmeticsMgr",
        "ApplySkinToShip",
        &[
            PyRep::Integer(active_ship_id),
            PyRep::Integer(i64::from(license_type_id)),
        ],
    )
    .await
    .expect("ApplySkinToShip");
    assert_eq!(
        apply_rep.dict_get("shipID"),
        Some(&PyRep::Integer(active_ship_id))
    );
    assert_eq!(
        apply_rep.dict_get("skinID"),
        Some(&PyRep::Integer(i64::from(resolved_skin_id)))
    );

    // Observer PILOT_2 already in the solar system receives slim item update + cosmetic change
    let observer_notifs = ctx.drain_notifications_for_char(PILOT_2);
    let slim_notif = observer_notifs
        .iter()
        .find(|n| n.event == "OnSlimItemChange")
        .expect("observer PILOT_2 must receive OnSlimItemChange");
    assert_eq!(
        slim_notif
            .args
            .get(1)
            .and_then(|s| s.dict_get("skinID"))
            .and_then(|v| v.as_i64()),
        Some(i64::from(resolved_skin_id)),
        "newSlim.skinID must carry the applied SDE skinID"
    );
    assert!(
        observer_notifs
            .iter()
            .any(|n| n.event == "OnShipCosmeticsChanged"),
        "observer PILOT_2 must receive OnShipCosmeticsChanged"
    );

    // Late-arriving grid observer syncing Ballpark state via beyonce.UpdateStateRequest sees skinID in SetState slims
    let late_observer_cid = 90_000_003i64;
    let late_ship_id = 100_999_003i64;
    let mut sess_late = crate::sovereignty_equinox_gameplay_closure::register_online_pilot(
        &ctx,
        39_903,
        "demo3",
        "demo3-secret",
        late_observer_cid,
        98_000_093,
        None,
        JITA_SYS,
        late_ship_id,
        587,
        DVec3::new(10_500.0, 0.0, 0.0),
        0,
    )
    .await;
    let _ = ctx.drain_notifications_for_char(late_observer_cid);
    let beyonce_oid = ctx
        .bind_object(&mut sess_late, "beyonce", i64::from(JITA_SYS))
        .expect("bind beyonce");
    ctx.dispatch_rpc(
        &mut sess_late,
        "beyonce",
        Some(beyonce_oid),
        "UpdateStateRequest",
        &[],
        &std::collections::HashMap::new(),
        None,
    )
    .await
    .expect("beyonce.UpdateStateRequest for late-arriving grid observer");
    let (_, _, late_set_state_rep) = sys_h
        .drain_observer_updates(late_ship_id)
        .await
        .expect("drain SetState for late observer");
    let late_destiny_dbg = format!("{late_set_state_rep:?}");
    assert!(
        late_destiny_dbg.contains("\"SetState\"")
            && late_destiny_dbg.contains(&format!(
                "(String(\"skinID\"), Integer({resolved_skin_id}))"
            )),
        "late-arriving observer SetState slim item must include skinID={resolved_skin_id}, got: {late_destiny_dbg}"
    );

    // Re-spawning the skinned ship on grid emits AddBalls2 with skinID in DoDestinyUpdate
    let _ = sys_h.remove_ball(active_ship_id).await;
    let _ = sys_h.drain_observer_updates(late_ship_id).await;
    let _ = sys_h
        .spawn_ball_with_meta(
            crate::sovereignty_equinox_gameplay_closure::make_ball(
                active_ship_id,
                587,
                DVec3::new(10_000.0, 0.0, 0.0),
            ),
            ctx.build_ship_combat_state(PILOT_1, active_ship_id, 587),
            ctx.build_ship_slim_meta(PILOT_1, 587),
        )
        .await;
    let (_, _, addballs2_rep) = sys_h
        .drain_observer_updates(late_ship_id)
        .await
        .expect("drain AddBalls2 for late observer");
    let addballs2_dbg = format!("{addballs2_rep:?}");
    assert!(
        addballs2_dbg.contains("\"AddBalls2\"")
            && addballs2_dbg.contains(&format!(
                "(String(\"skinID\"), Integer({resolved_skin_id}))"
            )),
        "AddBalls2 slim item must include skinID={resolved_skin_id}, got: {addballs2_dbg}"
    );

    // Verify canonical SDE skin_id persists across SQLite restart AND populates build_ship_slim_meta
    let ctx_re =
        crate::sp6_logoff_and_server_tick::persist_and_reopen_sqlite(&ctx, "b7_skin_apply");
    let persisted_skin = ctx_re
        .db
        .with_snapshot(|s| s.applied_ship_skins.get(&active_ship_id).copied());
    assert_eq!(persisted_skin, Some(resolved_skin_id));
    let reloaded_meta = ctx_re.build_ship_slim_meta(PILOT_1, 587);
    assert_eq!(
        reloaded_meta.skin_id,
        Some(resolved_skin_id),
        "build_ship_slim_meta after SQLite reload must populate canonical skin_id"
    );
}

#[tokio::test]
async fn test_structure_directory_accessible_services_and_acl() {
    // parity: feat:structures.structure-directory
    let ctx = ServerContext::new();
    let mut unauth = SessionState::new(997);
    assert!(
        rpc(&ctx, &mut unauth, "structureDirectory", "GetStructures", &[])
            .await
            .is_err(),
        "unauthenticated structureDirectory.GetStructures must fail"
    );

    let mut sess1 = setup_pilot(&ctx, PILOT_1).await;
    let mut sess2 = setup_pilot_for_user(&ctx, 77_012, "other", "other-secret", PILOT_2).await;
    let p3_uid = ctx
        .db
        .provision_player_account("b7_blocked", "b7-blocked-secret");
    let p3_char = ctx
        .db
        .create_character(p3_uid, "Batch7 Blocked", 1, 1, 11, 17, &ctx.sde)
        .expect("create blocked pilot")
        .character_id;
    let mut sess3 =
        setup_pilot_for_user(&ctx, 77_013, "b7_blocked", "b7-blocked-secret", p3_char).await;
    ctx.db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&p3_char) {
            ch.solar_system_id = 30_000_144; // Perimeter (not in Jita)
            ch.station_id = None;
        }
    });
    sess3.solar_system_id = Some(30_000_144);
    sess3.station_id = None;

    // Create owning player corporation for PILOT_1
    let corp_rep = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "AddCorporation",
        &[
            PyRep::String("Batch7 Upwell Holdings".into()),
            PyRep::String("B7UH".into()),
            PyRep::String("Structure directory corp".into()),
            PyRep::Real(0.05),
        ],
    )
    .await
    .expect("AddCorporation");
    let owner_corp = corp_rep
        .dict_get("corporationID")
        .and_then(|v| v.as_i64())
        .or_else(|| corp_rep.as_i64())
        .expect("owner_corp");
    sess1.corporation_id = Some(owner_corp);

    // Deploy an operational Raitaru (35825) with online Cloning (35894 -> 10) & Manufacturing (35878 -> 22) services
    let structure_id = 1_045_000_777_001i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            structure_id,
            UpwellStructureRecord {
                structure_id,
                type_id: 35_825,
                owner_corp_id: owner_corp,
                solar_system_id: JITA_SYS,
                name: "Jita - B7 Industrial Complex".to_string(),
                position: [150_000.0, 0.0, 0.0],
                state: "ShieldVulnerable".to_string(),
                quantum_core_installed: true,
                fuel_blocks: 500,
                shield_hp: 1.0,
                armor_hp: 1.0,
                hull_hp: 1.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
        for (iid, tid, flag) in [
            (990_800_001i64, 35_894u32, InventoryFlag::ServiceSlot0),
            (990_800_002i64, 35_878u32, InventoryFlag::ServiceSlot1),
        ] {
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: tid,
                    owner_id: owner_corp,
                    location_id: structure_id,
                    flag,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    dynamic_attributes: Default::default(),
                    source_mutaplasmid_id: None,
                    insured_payout_isk: 0.0,
                    is_blueprint_copy: false,
                    blueprint_runs_remaining: None,
                },
            );
            snap.module_online_states.insert(iid, true);
        }
    });

    // Configure ACL profile: PILOT_2 allowed, p3_char blocked
    eve_server::services::station_structure::structure_upwell_ops::configure_structure_access_profile(
        &ctx,
        &sess1,
        structure_id,
        Some(false),
        &[(10, 0.01), (22, 0.02)],
        &[],
        &[p3_char],
        &[PILOT_2],
    )
    .expect("configure_structure_access_profile");

    for sess in [&mut sess1, &mut sess2] {
        let dir_rep = rpc(&ctx, sess, "structureDirectory", "GetStructures", &[])
            .await
            .expect("GetStructures");
        let PyRep::Dict(entries) = dir_rep else {
            panic!("expected Dict from structureDirectory.GetStructures");
        };
        let entry = entries
            .iter()
            .find_map(|(k, v)| (k.as_i64() == Some(structure_id)).then_some(v))
            .expect("accessible structure must appear in GetStructures");
        let Some(PyRep::Dict(svcs)) = entry.dict_get("services") else {
            panic!("expected services Dict on structure directory entry");
        };
        assert!(svcs.contains(&(PyRep::Integer(10), PyRep::Integer(1))));
        assert!(svcs.contains(&(PyRep::Integer(22), PyRep::Integer(1))));
    }

    // Blocked pilot PILOT_3 does not see structure in GetStructures and is denied GetStructureInfo
    let blocked_dir = rpc(&ctx, &mut sess3, "structureDirectory", "GetStructures", &[])
        .await
        .expect("GetStructures blocked pilot");
    let PyRep::Dict(blocked_entries) = blocked_dir else {
        panic!("expected Dict");
    };
    assert!(
        !blocked_entries
            .iter()
            .any(|(k, _)| k.as_i64() == Some(structure_id)),
        "ACL-blocked pilot must not see restricted structure in GetStructures"
    );
    let info_err = rpc(
        &ctx,
        &mut sess3,
        "structureDirectory",
        "GetStructureInfo",
        &[PyRep::Integer(structure_id)],
    )
    .await
    .expect_err("ACL-blocked remote pilot must fail GetStructureInfo");
    assert_eq!(info_err.code, "StructureAccessDenied");

    // Verify SQLite persistence of structure directory & online services
    let ctx_re =
        crate::sp6_logoff_and_server_tick::persist_and_reopen_sqlite(&ctx, "b7_struct_dir");
    let mut sess2_re =
        setup_pilot_for_user(&ctx_re, 77_022, "other", "other-secret", PILOT_2).await;
    let dir_re = rpc(&ctx_re, &mut sess2_re, "structureDirectory", "GetStructures", &[])
        .await
        .expect("GetStructures after reload");
    let PyRep::Dict(re_entries) = dir_re else {
        panic!("expected Dict after reload");
    };
    assert!(re_entries
        .iter()
        .any(|(k, _)| k.as_i64() == Some(structure_id)));
}
