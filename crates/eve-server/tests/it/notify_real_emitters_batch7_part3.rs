//! Real server-side emission tests for Notify Batch 7 (Part 3: Cosmetics, Petitions,
//! Mail Undelete, Planetary Interaction Launches, Orbital State & Escalations).

use crate::notify_real_emitters_batch7::{
    drain_all, find_notif, login_in_space, make_item, new_ctx, rpc, JITA_STATION, PILOT_A,
    PILOT_B, SYS_JITA,
};
use crate::quasar_goal_tale_notices::{seq, subscribe};
use crate::quasar_progression_complete::{build_sess, quasar_rpc};
use eve_db::{InventoryFlag, IskCents, TrainedSkillRecord};
use eve_destiny::DamageVector;
use eve_proto::quasar::{encode_bytes_field, encode_varint_field};
use eve_proto::PyRep;
use eve_server::quasar_gateway::STATUS_OK;
use eve_server::services::pve_exploration::with_pve_exploration_reg;
use glam::DVec3;

// parity: notify:OnSkinLicenseActivated, notify:OnSkinLicenseRemoved
// parity: notify:OnShipCosmeticChanged, notify:OnShipCosmeticsChanged
// parity: notify:OnNewPetition, notify:OnAssignPetition
// parity: notify:OnPetitionMessage, notify:OnClosePetition
// parity: notify:OnMailUndeleted, notify:OnPILaunchesChange
// parity: notify:OnMajorPlanetStateUpdate, notify:OnEscalatingPathMessage
#[tokio::test]
async fn notify_batch7_cosmetics_petitions_mail_pi_and_escalations() {
    eve_testkit::attest(&[
        "notify:OnSkinLicenseActivated",
        "notify:OnSkinLicenseRemoved",
        "notify:OnShipCosmeticChanged",
        "notify:OnShipCosmeticsChanged",
        "notify:OnNewPetition",
        "notify:OnAssignPetition",
        "notify:OnPetitionMessage",
        "notify:OnClosePetition",
        "notify:OnMailUndeleted",
        "notify:OnPILaunchesChange",
        "notify:OnMajorPlanetStateUpdate",
        "notify:OnEscalatingPathMessage",
    ]);

    let ctx = new_ctx();
    let (ship_a, ship_b) = (730_000_001_i64, 730_000_002_i64);
    let mut sess_a = login_in_space(&ctx, 7_301, "demo", "demo-secret", PILOT_A, ship_a, 587, SYS_JITA, DVec3::ZERO).await;
    let mut sess_b = login_in_space(&ctx, 7_302, "other", "other-secret", PILOT_B, ship_b, 587, SYS_JITA, DVec3::new(1_000.0, 0.0, 0.0)).await;

    // 1-2. OnSkinLicenseActivated (2-arg) & OnSkinLicenseRemoved (3-arg on timed SKIN expiry)
    let (perm_lic_tid, timed_lic_tid) = (34_748_u32, 35_493_u32);
    let perm_skin_id = ctx.sde.skin_licenses.get(&perm_lic_tid).expect("34748 in SDE").skin_id;
    let timed_lic = ctx.sde.skin_licenses.get(&timed_lic_tid).expect("35493 in SDE");
    let (timed_skin_id, timed_days) = (timed_lic.skin_id, timed_lic.duration);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
            ch.station_id = Some(JITA_STATION);
        }
        snap.items.insert(730_000_011, make_item(730_000_011, perm_lic_tid, PILOT_A, JITA_STATION, InventoryFlag::Hangar, 1, false));
        snap.items.insert(730_000_012, make_item(730_000_012, timed_lic_tid, PILOT_A, JITA_STATION, InventoryFlag::Hangar, 1, false));
    });
    sess_a.station_id = Some(JITA_STATION);
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);

    rpc(&ctx, &mut sess_a, "shipCosmeticsMgr", "ActivateSkinLicense", &[PyRep::Integer(perm_lic_tid as i64)])
        .await
        .expect("Activate permanent SKIN");
    let skin_act_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert_eq!(
        find_notif(&skin_act_notifs, "OnSkinLicenseActivated"),
        &[PyRep::Integer(perm_skin_id as i64), PyRep::Integer(PILOT_A)]
    );

    rpc(&ctx, &mut sess_a, "shipCosmeticsMgr", "ActivateSkinLicense", &[PyRep::Integer(timed_lic_tid as i64)])
        .await
        .expect("Activate timed SKIN");
    rpc(&ctx, &mut sess_a, "shipCosmeticsMgr", "ApplySkinToShip", &[PyRep::Integer(ship_a), PyRep::Integer(timed_lic_tid as i64)])
        .await
        .expect("Apply timed SKIN to ship");
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);

    ctx.advance_secs((timed_days as u64 + 1) * 86_400);
    rpc(&ctx, &mut sess_a, "shipCosmeticsMgr", "GetLicencedSkins", &[])
        .await
        .expect("GetLicencedSkins after expiry");
    let skin_rm_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert_eq!(
        find_notif(&skin_rm_notifs, "OnSkinLicenseRemoved"),
        &[PyRep::Integer(timed_skin_id as i64), PyRep::Integer(ship_a), PyRep::Integer(587)]
    );

    // 3-4. OnShipCosmeticChanged (3-arg) & OnShipCosmeticsChanged (2-arg) via Quasar ship logo DisplayRequest
    let corp_a = ctx.db.with_snapshot(|s| s.characters[&PILOT_A].corporation_id as u32);
    let mut gm_q = build_sess(7_399, PILOT_A as u32, corp_a);
    gm_q.user_id = Some(7_399);
    let mut player_q = build_sess(7_398, PILOT_A as u32, corp_a);
    player_q.user_id = Some(7_398);
    player_q.role = eve_machonet::service_roles::ROLE_PLAYER_DEFAULT;
    let _rx_q = subscribe(&ctx, &mut player_q).await;

    let wrap = |field: u32, payload: &[u8]| {
        let mut out = Vec::new();
        encode_bytes_field(field, payload, &mut out);
        out
    };
    let logo_ent = eve_server::services::quasar_entitlement_membership::encode_logo_identifier(PILOT_A as u64, 587);
    assert_eq!(
        quasar_rpc(&ctx, &mut gm_q, "eve_public.entitlement.character.ship.admin.corplogo.GrantRequest", wrap(1, &logo_ent))
            .await
            .status_code,
        STATUS_OK
    );
    let mut id_bytes = Vec::new();
    seq(1, ship_a as u64, &mut id_bytes);
    encode_varint_field(2, 0, &mut id_bytes);
    let mut logo_req = wrap(1, &id_bytes);
    encode_bytes_field(2, &wrap(2, &[]), &mut logo_req);
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert_eq!(
        quasar_rpc(&ctx, &mut player_q, "eve_public.cosmetic.ship.logo.DisplayRequest", logo_req)
            .await
            .status_code,
        STATUS_OK
    );
    let cos_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    let cos_single = find_notif(&cos_notifs, "OnShipCosmeticChanged");
    assert_eq!(cos_single, &[PyRep::Integer(ship_a), PyRep::Integer(0), PyRep::Integer(1)]);
    let cos_map = find_notif(&cos_notifs, "OnShipCosmeticsChanged");
    assert_eq!(
        cos_map,
        &[PyRep::Integer(ship_a), PyRep::Dict(vec![(PyRep::Integer(0), PyRep::Integer(1))])]
    );

    // 5-8. OnNewPetition (1-arg), OnAssignPetition (2-arg), OnPetitionMessage (4-arg), OnClosePetition (1-arg)
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
    let pet_rep = rpc(
        &ctx,
        &mut sess_a,
        "petitioner",
        "CreatePetition",
        &[PyRep::String("Batch 7 Ticket".into()), PyRep::String("Need assistance".into()), PyRep::Integer(1)],
    )
    .await
    .expect("CreatePetition");
    let pet_id = pet_rep.dict_get("petitionID").and_then(|v| v.as_i64()).expect("petitionID");
    let new_pet_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert_eq!(find_notif(&new_pet_notifs, "OnNewPetition"), &[PyRep::Integer(pet_id)]);

    sess_b.role = eve_machonet::service_roles::ROLE_GM_DEFAULT;
    rpc(&ctx, &mut sess_b, "petitioner", "ClaimPetition", &[PyRep::Integer(pet_id)])
        .await
        .expect("ClaimPetition");
    let claim_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert_eq!(find_notif(&claim_notifs, "OnAssignPetition"), &[PyRep::Integer(pet_id), PyRep::Integer(PILOT_B)]);

    rpc(&ctx, &mut sess_a, "petitioner", "PetitionerChat", &[PyRep::Integer(pet_id), PyRep::String("Follow-up info".into())])
        .await
        .expect("PetitionerChat");
    let msg_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    let msg_args = find_notif(&msg_notifs, "OnPetitionMessage");
    assert_eq!(msg_args.len(), 4);
    assert_eq!(msg_args[0], PyRep::Integer(pet_id));
    assert_eq!(msg_args[1], PyRep::Integer(PILOT_A));
    assert_eq!(msg_args[2], PyRep::String("Follow-up info".into()));
    assert!(msg_args[3].as_i64().unwrap_or(0) > 0);

    rpc(&ctx, &mut sess_b, "petitioner", "ClosePetition", &[PyRep::Integer(pet_id)])
        .await
        .expect("ClosePetition");
    let close_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert_eq!(find_notif(&close_notifs, "OnClosePetition"), &[PyRep::Integer(pet_id)]);

    // 9. OnMailUndeleted (1-arg list of messageIDs) via MoveFromTrash
    let mail_rep = rpc(
        &ctx,
        &mut sess_a,
        "mailMgr",
        "SendMail",
        &[
            PyRep::List(vec![PyRep::Integer(PILOT_B)]),
            PyRep::Integer(0),
            PyRep::None,
            PyRep::String("Batch 7 Mail".into()),
            PyRep::String("Restore test".into()),
        ],
    )
    .await
    .expect("SendMail");
    let mail_id = mail_rep.as_i64().expect("mail_id");
    rpc(&ctx, &mut sess_b, "mailMgr", "MoveToTrash", &[PyRep::Integer(mail_id)])
        .await
        .expect("MoveToTrash");
    let _ = drain_all(&ctx, &mut sess_b, PILOT_B);
    rpc(&ctx, &mut sess_b, "mailMgr", "MoveFromTrash", &[PyRep::Integer(mail_id)])
        .await
        .expect("MoveFromTrash");
    let mail_notifs = drain_all(&ctx, &mut sess_b, PILOT_B);
    assert_eq!(find_notif(&mail_notifs, "OnMailUndeleted"), &[PyRep::List(vec![PyRep::Integer(mail_id)])]);

    // 10-11. OnPILaunchesChange (0-arg) & OnMajorPlanetStateUpdate (2-arg)
    let jita_planet_id = 40_009_077_i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
            ch.station_id = None;
            ch.solar_system_id = SYS_JITA;
            ch.balance = IskCents::from_isk(100_000_000);
        }
        let sk = snap.skills.entry(PILOT_A).or_default();
        sk.insert(2505, TrainedSkillRecord { type_id: 2505, level: 5, skill_points: 256_000.0 });
        sk.insert(2495, TrainedSkillRecord { type_id: 2495, level: 4, skill_points: 256_000.0 });
        snap.items.insert(730_000_041, make_item(730_000_041, 2524, PILOT_A, ship_a, InventoryFlag::Cargo, 1, false));
        snap.items.insert(730_000_042, make_item(730_000_042, 2389, PILOT_A, ship_a, InventoryFlag::Cargo, 500, false));
    });
    sess_a.station_id = None;
    sess_a.solar_system_id = Some(SYS_JITA);
    sess_a.corp_role = eve_db::corp_roles::ROLE_DIRECTOR;

    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
    rpc(
        &ctx,
        &mut sess_a,
        "planetMgr",
        "UserUpdateNetwork",
        &[
            PyRep::Integer(jita_planet_id),
            PyRep::List(vec![
                PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(1001), PyRep::Integer(2524), PyRep::Real(0.10), PyRep::Real(0.10)]),
            ]),
        ],
    )
    .await
    .expect("Deploy PI colony");
    let deploy_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert_eq!(
        find_notif(&deploy_notifs, "OnMajorPlanetStateUpdate"),
        &[PyRep::Integer(jita_planet_id), PyRep::Bool(true)]
    );

    rpc(
        &ctx,
        &mut sess_a,
        "planetMgr",
        "UserLaunchCommodities",
        &[PyRep::Integer(jita_planet_id), PyRep::Integer(2389), PyRep::Integer(300), PyRep::String("ImportToColony".into())],
    )
    .await
    .expect("ImportToColony");
    let import_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert!(!import_notifs.iter().any(|(e, _)| e == "OnPILaunchesChange"));

    let launch_rep = rpc(
        &ctx,
        &mut sess_a,
        "planetMgr",
        "UserLaunchCommodities",
        &[PyRep::Integer(jita_planet_id), PyRep::Integer(2389), PyRep::Integer(100), PyRep::String("CustomsOffice".into())],
    )
    .await
    .expect("Launch to CustomsOffice");
    let launch_id = launch_rep.dict_get("launchID").and_then(|v| v.as_i64()).expect("launchID");
    let pi_launch_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert_eq!(find_notif(&pi_launch_notifs, "OnPILaunchesChange"), &[]);

    rpc(&ctx, &mut sess_a, "planetMgr", "DeleteLaunch", &[PyRep::Integer(launch_id)])
        .await
        .expect("DeleteLaunch");
    let pi_del_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert_eq!(find_notif(&pi_del_notifs, "OnPILaunchesChange"), &[]);

    sess_a.role = eve_machonet::service_roles::ROLE_GM_DEFAULT;
    rpc(&ctx, &mut sess_a, "planetOrbitalRegistryBroker", "UpdateSettings", &[PyRep::Integer(jita_planet_id)])
        .await
        .expect("UpdateSettings");
    let orb_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert_eq!(
        find_notif(&orb_notifs, "OnMajorPlanetStateUpdate"),
        &[PyRep::Integer(jita_planet_id), PyRep::Bool(false)]
    );

    rpc(&ctx, &mut sess_a, "planetMgr", "UserAbandonPlanet", &[PyRep::Integer(jita_planet_id)])
        .await
        .expect("UserAbandonPlanet");
    let abandon_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert_eq!(
        find_notif(&abandon_notifs, "OnMajorPlanetStateUpdate"),
        &[PyRep::Integer(jita_planet_id), PyRep::Bool(true)]
    );

    // 12. OnEscalatingPathMessage (1-arg instanceID) via combat anomaly escalation
    let _ = rpc(&ctx, &mut sess_a, "scanSvc", "GetCombatAnomalyInstances", &[PyRep::Integer(SYS_JITA as i64)])
        .await
        .expect("GetCombatAnomalyInstances");
    let site_id = "PVE-ANOM-30000142-1";
    async fn clear_wave(ctx: &eve_server::router::ServerContext, sid: &'static str) {
        let ball_ids = with_pve_exploration_reg(ctx, |reg| {
            reg.sites_by_system
                .get(&SYS_JITA)
                .and_then(|l| l.iter().find(|s| s.site_id == sid))
                .map(|s| s.active_npc_ball_ids.clone())
                .unwrap_or_default()
        });
        let jita = ctx.systems.get_or_spawn(SYS_JITA);
        for bid in ball_ids {
            let _ = jita
                .apply_damage(bid, DamageVector { em: 50_000.0, thermal: 50_000.0, kinetic: 50_000.0, explosive: 50_000.0 })
                .await;
        }
    }
    clear_wave(&ctx, site_id).await;
    rpc(&ctx, &mut sess_a, "scanSvc", "EnterCombatAnomaly", &[PyRep::String(site_id.into()), PyRep::Bool(true)])
        .await
        .expect("EnterCombatAnomaly wave 2");
    clear_wave(&ctx, site_id).await;
    // Seed 3 gives SplitMix64 first draw 0.1135 < 0.20 (natural 20% escalation roll)
    ctx.set_mechanic_rng_seed(Some(3));
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
    rpc(&ctx, &mut sess_a, "scanSvc", "EnterCombatAnomaly", &[PyRep::String(site_id.into()), PyRep::Bool(true)])
        .await
        .expect("Complete anomaly with escalation");
    let esc_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    let esc_args = find_notif(&esc_notifs, "OnEscalatingPathMessage");
    assert_eq!(esc_args.len(), 1);
    assert!(esc_args[0].as_i64().unwrap_or(0) > 0);
}
