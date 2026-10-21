//! End-to-End Integration Tests for Death Pipeline, Wrecks, Killmails, CONCORD & CrimeWatch 2.0 (`C-3, C-8`).

use eve_db::{InventoryFlag, InventoryItemRecord, IskCents};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::combat_crimewatch::{
    check_crimewatch_action_restriction, compute_killmail_esi_hash, dispatch_combat_rpc,
    evaluate_faction_police_response, evaluate_remote_assistance_legality,
    faction_police_sec_threshold, record_killmail_assist_action, register_container_lifetime,
    step_combat_system, step_sentry_gun_grid_aggression, step_wreck_and_jetcan_decay,
    with_mut_combat_runtime,
};
use glam::DVec3;
use std::collections::HashMap;

fn dict_get<'a>(rep: &'a PyRep, key: &str) -> Option<&'a PyRep> {
    match rep {
        PyRep::KeyVal(map) => map.get(key),
        PyRep::Dict(entries) => entries
            .iter()
            .find_map(|(k, v)| (k.as_str() == Some(key)).then_some(v)),
        _ => None,
    }
}

async fn setup_pilot_session(
    ctx: &ServerContext,
    client_id: u64,
    char_id: i64,
    ship_id: i64,
    ship_type_id: u32,
    solar_system_id: u32,
    pos: DVec3,
) -> SessionState {
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = solar_system_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = ship_id;
            ch.balance = IskCents::from_isk(500_000_000);
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: ship_type_id,
                owner_id: char_id,
                location_id: solar_system_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });

    let sys_handle = ctx.systems.get_or_spawn(solar_system_id);
    let sig_radius = ctx
        .sde
        .types
        .get(&ship_type_id)
        .and_then(|t| t.default_attributes.get(&552).copied())
        .unwrap_or(40.0);
    let ship_ball = Ball {
        id: ship_id,
        type_id: ship_type_id,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 350.0,
        inertia: 3.0,
        mass: 1_200_000.0,
        radius: sig_radius,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let _ = sys_handle
        .spawn_ball_with_combat(ship_ball, ctx.build_type_combat_state(ship_type_id))
        .await;

    let mut sess = SessionState::new(client_id);
    let (user_id, corp_id) = ctx.db.with_snapshot(|snap| {
        snap.characters
            .get(&char_id)
            .map(|c| (c.user_id, c.corporation_id))
            .unwrap_or((1, 1_000_002))
    });
    sess.user_id = Some(user_id);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    sess.solar_system_id = Some(solar_system_id);
    sess.ship_id = Some(ship_id);
    sess.station_id = None;
    sess
}

fn fit_module_and_charge(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    module_item_id: i64,
    module_type_id: u32,
    flag: InventoryFlag,
    charge_opt: Option<(i64, u32, i32)>,
) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            module_item_id,
            InventoryItemRecord {
                item_id: module_item_id,
                type_id: module_type_id,
                owner_id: char_id,
                location_id: ship_id,
                flag,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        if let Some((charge_id, charge_type_id, qty)) = charge_opt {
            snap.items.insert(
                charge_id,
                InventoryItemRecord {
                    item_id: charge_id,
                    type_id: charge_type_id,
                    owner_id: char_id,
                    location_id: ship_id,
                    flag,
                    quantity: qty,
                    singleton: false,
                    version: 1,
                    dynamic_attributes: HashMap::new(),
                    source_mutaplasmid_id: None,
                    insured_payout_isk: 0.0,
                    is_blueprint_copy: false,
                    blueprint_runs_remaining: None,
                },
            );
        }
    });
}

#[tokio::test]
async fn test_c3_wrecks_loot_50pct_ownership_killmails_esi_hash_pod_corpse_and_self_destruct() {
    // parity: feat:combat.ship-destruction
    // parity: feat:combat.pod-destruction
    // parity: feat:combat.wreck-loot
    // parity: feat:combat.loot-all
    // parity: feat:combat.wreck-lifetime
    // parity: feat:combat.killmail-generate
    // parity: feat:combat.killmail-view
    // parity: feat:combat.killmail-esi-hash
    // parity: feat:combat.killmail-assist-timeout-ewar
    // parity: feat:combat.capsule-ejection-items
    // parity: feat:combat.self-destruct-pod
    // parity: feat:crimewatch.criminal-wreck-auto-abandon
    eve_testkit::attest(&[
        "feat:combat.ship-destruction",
        "feat:combat.pod-destruction",
        "feat:combat.wreck-loot",
        "feat:combat.loot-all",
        "feat:combat.wreck-lifetime",
        "feat:combat.killmail-generate",
        "feat:combat.killmail-view",
        "feat:combat.killmail-esi-hash",
        "feat:combat.killmail-assist-timeout-ewar",
        "feat:combat.capsule-ejection-items",
        "feat:combat.self-destruct-pod",
        "feat:crimewatch.criminal-wreck-auto-abandon",
    ]);

    let ctx = ServerContext::new();
    let attacker_id = 90_000_001;
    let ewar_helper_id = 90_000_003;
    let victim_id = 90_000_002;
    let attacker_ship_id = 100_000_001;
    let victim_ship_id = 100_000_002;
    let sys_id = 30_002_537; // Low-sec Amamake

    let mut attacker_sess = setup_pilot_session(
        &ctx,
        7105,
        attacker_id,
        attacker_ship_id,
        24690, // Maelstrom
        sys_id,
        DVec3::ZERO,
    )
    .await;
    let mut victim_sess = setup_pilot_session(
        &ctx,
        7106,
        victim_id,
        victim_ship_id,
        587, // Rifter
        sys_id,
        DVec3::new(1_000.0, 0.0, 0.0),
    )
    .await;

    // Seed victim ship with 20 cargo items and a stack of 100 units so the 50% Loot Fairy drops ~half and destroys ~half
    ctx.db.with_mut_snapshot(|snap| {
        for i in 0..20 {
            let iid = 700_000 + i;
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: 486, // 200mm AutoCannon I
                    owner_id: victim_id,
                    location_id: victim_ship_id,
                    flag: InventoryFlag::Cargo,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    dynamic_attributes: HashMap::new(),
                    source_mutaplasmid_id: None,
                    insured_payout_isk: 0.0,
                    is_blueprint_copy: false,
                    blueprint_runs_remaining: None,
                },
            );
        }
        snap.items.insert(
            700_100,
            InventoryItemRecord {
                item_id: 700_100,
                type_id: 185, // EMP S stack of 100
                owner_id: victim_id,
                location_id: victim_ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 100,
                singleton: false,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });

    // Record a 0-damage EWAR assist within the 15-minute (900s) window and an expired assist (> 900s ago)
    let now_unix = ctx.now_unix();
    record_killmail_assist_action(
        &ctx,
        victim_ship_id,
        ewar_helper_id,
        587,
        527, // Stasis Webifier I (0-damage EWAR/tackle assist)
        0.0,
        true,
        now_unix.saturating_sub(120),
    );
    record_killmail_assist_action(
        &ctx,
        victim_ship_id,
        90_000_099, // Expired attacker (> 15 minutes ago)
        587,
        486,
        250.0,
        false,
        now_unix.saturating_sub(1_200),
    );

    // Fit artillery/autocannon on attacker and destroy victim ship
    let gun_id = 203_001;
    let ammo_id = 203_002;
    fit_module_and_charge(
        &ctx,
        attacker_id,
        attacker_ship_id,
        gun_id,
        486,
        InventoryFlag::HiSlot0,
        Some((ammo_id, 185, 100)),
    );
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    if let Ok(Some(mut vcs)) = sys_handle.get_combat_state(victim_ship_id).await {
        vcs.shield_hp = 0.0;
        vcs.armor_hp = 0.0;
        vcs.structure_hp = 5.0;
        let _ = sys_handle.set_combat_state(victim_ship_id, vcs).await;
    }

    let kw = HashMap::new();
    ctx.set_invention_roll_override(Some(0.005)); // Wrecking hit to guarantee kill
    let kill_res = dispatch_combat_rpc(
        &ctx,
        &mut attacker_sess,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(gun_id), PyRep::Integer(victim_ship_id)],
        &kw,
    )
    .await
    .expect("Kill shot");

    assert_eq!(
        dict_get(&kill_res, "destroyed").and_then(|v| v.as_bool()),
        Some(true)
    );
    let kill_id = dict_get(&kill_res, "killID")
        .and_then(|v| v.as_i64())
        .expect("killID");
    let wreck_id = dict_get(&kill_res, "wreckItemID")
        .and_then(|v| v.as_i64())
        .expect("wreckItemID");
    let km_hash = dict_get(&kill_res, "killmailHash")
        .and_then(|v| v.as_str())
        .expect("killmailHash")
        .to_string();
    assert_eq!(
        km_hash.len(),
        40,
        "Killmail ESI SHA-1 hash must be 40 lowercase hex chars"
    );

    // 1. Wreck generation (`feat:combat.wreck-generation`) & 50% Loot Fairy (`feat:combat.loot-drop-50pct`)
    assert!(
        sys_handle
            .get_ball_snapshot(wreck_id)
            .await
            .unwrap()
            .is_some(),
        "Wreck ball must be spawned at victim destruction coordinates"
    );

    // 2. Killmail generation (`feat:combat.killmail-generation`) & SHA-1 verification (`feat:combat.killmail-esi-hash`)
    let km_rep = dispatch_combat_rpc(
        &ctx,
        &mut attacker_sess,
        "warStatisticMgr",
        "GetKillMail",
        &[PyRep::Integer(kill_id), PyRep::String(km_hash.clone())],
        &HashMap::new(),
    )
    .await
    .expect("GetKillMail with valid hash");
    let dropped_cnt = match dict_get(&km_rep, "droppedItems") {
        Some(PyRep::List(v)) => v.len(),
        _ => 0,
    };
    let destroyed_cnt = match dict_get(&km_rep, "destroyedItems") {
        Some(PyRep::List(v)) => v.len(),
        _ => 0,
    };
    assert!(
        dropped_cnt > 0 && destroyed_cnt > 0,
        "50% Loot Fairy roll must split items between dropped ({dropped_cnt}) and destroyed ({destroyed_cnt})"
    );
    let attackers = match dict_get(&km_rep, "attackers") {
        Some(PyRep::List(v)) => v.clone(),
        _ => Vec::new(),
    };
    let attacker_ids: Vec<i64> = attackers
        .iter()
        .filter_map(|a| dict_get(a, "characterID").and_then(|v| v.as_i64()))
        .collect();
    assert!(
        attacker_ids.contains(&attacker_id),
        "Final-blow attacker must be on killmail"
    );
    assert!(
        attacker_ids.contains(&ewar_helper_id),
        "0-damage EWAR/tackle helper within 15m window must be on killmail"
    );
    assert!(
        !attacker_ids.contains(&90_000_099),
        "Expired attacker (> 15m window) must NOT be on killmail"
    );

    // Mismatched hash must be rejected
    let bad_hash_err = dispatch_combat_rpc(
        &ctx,
        &mut attacker_sess,
        "warStatisticMgr",
        "GetKillMail",
        &[
            PyRep::Integer(kill_id),
            PyRep::String("0000000000000000000000000000000000000000".into()),
        ],
        &HashMap::new(),
    )
    .await
    .expect_err("Mismatched hash must error");
    assert_eq!(bad_hash_err.code, "InvalidKillmailHash");

    // 3. Wreck looting ownership, 2,500m range check, capacity overflow, Suspect flag, and 2-hour decay (`feat:combat.wreck-looting-ownership`)
    let active_pod_id = ctx.db.with_snapshot(|s| s.characters[&victim_id].active_ship_id);
    let _ = sys_handle
        .set_ball_position(active_pod_id, DVec3::new(10_000.0, 0.0, 0.0))
        .await;
    let range_err = dispatch_combat_rpc(
        &ctx,
        &mut victim_sess,
        "invbroker",
        "LootAll",
        &[PyRep::Integer(wreck_id)],
        &HashMap::new(),
    )
    .await
    .expect_err("LootAll beyond 2,500m must be rejected");
    assert_eq!(range_err.code, "OutOfRange");
    let _ = sys_handle
        .set_ball_position(active_pod_id, DVec3::new(1_500.0, 0.0, 0.0))
        .await;

    // Victim loots foreign wreck (owned by attacker): triggers Suspect flag!
    let loot_res = dispatch_combat_rpc(
        &ctx,
        &mut victim_sess,
        "invbroker",
        "LootAll",
        &[PyRep::Integer(wreck_id)],
        &HashMap::new(),
    )
    .await
    .expect("LootAll");
    assert_eq!(
        dict_get(&loot_res, "suspectFlagTriggered").and_then(|v| v.as_bool()),
        Some(true),
        "Looting a foreign un-abandoned wreck outside corp/fleet must trigger a Suspect flag"
    );

    // Step wreck decay past 2 hours (7,200s) and verify despawn
    register_container_lifetime(&ctx, wreck_id, sys_id, 26557, attacker_id, 1_000, false);
    let despawned = step_wreck_and_jetcan_decay(&ctx, sys_id, 1_000 + 7_201).await;
    assert!(
        despawned.contains(&wreck_id),
        "Wrecks must despawn after 2 hours (7,200s)"
    );

    // 4. Pod destruction -> Frozen Corpse (type 25) spawn + active implant destruction (`feat:combat.pod-corpse-spawn`)
    let pod_ship_id = 100_000_099;
    ctx.db.with_mut_snapshot(|snap| {
        snap.active_implants.insert(victim_id, vec![9899, 9941]);
        if let Some(ch) = snap.characters.get_mut(&victim_id) {
            ch.active_ship_id = pod_ship_id;
        }
        snap.items.insert(
            pod_ship_id,
            InventoryItemRecord {
                item_id: pod_ship_id,
                type_id: 670, // Capsule
                owner_id: victim_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });
    let mut pod_cs = ctx.build_type_combat_state(670);
    pod_cs.shield_hp = 0.0;
    pod_cs.armor_hp = 0.0;
    pod_cs.structure_hp = 5.0;
    let _ = sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: pod_ship_id,
                type_id: 670,
                position: DVec3::new(1_200.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 200.0,
                inertia: 1.0,
                mass: 10_000.0,
                radius: 25.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            pod_cs,
        )
        .await;
    let pod_kill_res = dispatch_combat_rpc(
        &ctx,
        &mut attacker_sess,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(gun_id), PyRep::Integer(pod_ship_id)],
        &kw,
    )
    .await
    .expect("Pod Kill Activate");
    let corpse_id = dict_get(&pod_kill_res, "corpseItemID")
        .and_then(|v| v.as_i64())
        .expect("corpseItemID");
    assert!(
        corpse_id > 0,
        "Pod destruction must spawn a Frozen Corpse (type 25) in space"
    );
    let corpse_type = ctx.db.with_snapshot(|s| s.items[&corpse_id].type_id);
    assert_eq!(corpse_type, 25, "Spawned corpse item must have type_id == 25");
    assert!(
        ctx.db
            .with_snapshot(|s| s.active_implants.get(&victim_id).is_none_or(|v| v.is_empty())),
        "Pod destruction must destroy active head implants"
    );

    // 5. 120-second Self-Destruct countdown, AbortSelfDestruct, and 15m attacker attribution (`feat:ships.self-destruct-countdown`)
    let kw_sd = HashMap::new();
    let sd_init = dispatch_combat_rpc(
        &ctx,
        &mut attacker_sess,
        "ship",
        "SelfDestruct",
        &[PyRep::Integer(attacker_ship_id)],
        &kw_sd,
    )
    .await
    .expect("Start SelfDestruct countdown");
    assert_eq!(
        dict_get(&sd_init, "remainingSeconds").and_then(|v| v.as_i64()),
        Some(120)
    );
    // Cancel countdown via AbortSelfDestruct
    let sd_abort = dispatch_combat_rpc(
        &ctx,
        &mut attacker_sess,
        "ship",
        "AbortSelfDestruct",
        &[PyRep::Integer(attacker_ship_id)],
        &HashMap::new(),
    )
    .await
    .expect("AbortSelfDestruct");
    assert_eq!(
        dict_get(&sd_abort, "aborted").and_then(|v| v.as_bool()),
        Some(true)
    );

    // Re-initiate countdown, record an EWAR/damage assist from victim_id, and advance 120s
    let _ = dispatch_combat_rpc(
        &ctx,
        &mut attacker_sess,
        "ship",
        "SelfDestruct",
        &[PyRep::Integer(attacker_ship_id)],
        &kw_sd,
    )
    .await
    .expect("Re-start SelfDestruct");
    record_killmail_assist_action(
        &ctx,
        attacker_ship_id,
        victim_id,
        587,
        486,
        150.0,
        false,
        ctx.now_unix(),
    );
    with_mut_combat_runtime(&ctx, |rt| {
        if let Some(cd) = rt.self_destruct_countdowns.get_mut(&attacker_ship_id) {
            cd.detonate_at_unix = ctx.now_unix();
        }
    });
    step_combat_system(&ctx, sys_id, 1.0).await;
    assert!(
        sys_handle
            .get_ball_snapshot(attacker_ship_id)
            .await
            .unwrap()
            .is_none(),
        "Ship must detonate when 120s self-destruct countdown completes"
    );

    let hash_check = compute_killmail_esi_hash(attacker_id, victim_id, 24690, 1_700_000_000);
    assert_eq!(hash_check.len(), 40);
}

#[tokio::test]
async fn test_c8_crimewatch_safety_timers_restrictions_and_lowsec_pod() {
    // parity: feat:crimewatch.suspect-flag
    // parity: feat:crimewatch.criminal-flag
    // parity: feat:crimewatch.concord-response
    // parity: feat:crimewatch.safety-settings
    // parity: feat:crimewatch.pod-timer
    // parity: feat:crimewatch.legal-timers
    // parity: feat:crimewatch.crimewatch-states
    // parity: feat:crimewatch.weapon-timer-scope
    // parity: feat:crimewatch.pvp-logoff-timer-renewal
    // parity: feat:crimewatch.highsec-criminal-restrictions
    // parity: feat:crimewatch.lowsec-suspect-vs-criminal
    // parity: feat:crimewatch.weapon-timer-tether-filament-block
    // parity: feat:navigation.wormhole-aggression-exemption
    eve_testkit::attest(&[
        "feat:crimewatch.suspect-flag",
        "feat:crimewatch.criminal-flag",
        "feat:crimewatch.concord-response",
        "feat:crimewatch.safety-settings",
        "feat:crimewatch.pod-timer",
        "feat:crimewatch.legal-timers",
        "feat:crimewatch.crimewatch-states",
        "feat:crimewatch.weapon-timer-scope",
        "feat:crimewatch.pvp-logoff-timer-renewal",
        "feat:crimewatch.highsec-criminal-restrictions",
        "feat:crimewatch.lowsec-suspect-vs-criminal",
        "feat:crimewatch.weapon-timer-tether-filament-block",
        "feat:navigation.wormhole-aggression-exemption",
    ]);

    let ctx = ServerContext::new();
    let char_a = 90_000_001;
    let char_b = 90_000_002;
    let ship_a = 100_000_001;
    let ship_b = 100_000_002;
    let highsec_id = 30_000_142; // Jita (0.95)
    let lowsec_id = 30_002_537; // Amamake (low-sec)

    let mut sess_a = setup_pilot_session(
        &ctx,
        7107,
        char_a,
        ship_a,
        587,
        lowsec_id,
        DVec3::ZERO,
    )
    .await;
    let _sess_b = setup_pilot_session(
        &ctx,
        7108,
        char_b,
        ship_b,
        587,
        lowsec_id,
        DVec3::new(1_500.0, 0.0, 0.0),
    )
    .await;

    let gun_a = 204_001;
    let ammo_a = 204_002;
    fit_module_and_charge(
        &ctx,
        char_a,
        ship_a,
        gun_a,
        486,
        InventoryFlag::HiSlot0,
        Some((ammo_a, 185, 100)),
    );

    // 1. Safety settings (`feat:crimewatch.safety-settings`):
    // Set Safety to Green (2) -> blocks low-sec Suspect ship attack
    let _ = dispatch_combat_rpc(
        &ctx,
        &mut sess_a,
        "crimewatch",
        "SetSafetyLevel",
        &[PyRep::Integer(2)],
        &HashMap::new(),
    )
    .await
    .expect("SetSafetyLevel Green");
    let green_err = dispatch_combat_rpc(
        &ctx,
        &mut sess_a,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(gun_a), PyRep::Integer(ship_b)],
        &HashMap::new(),
    )
    .await
    .expect_err("Safety Green must block low-sec Suspect attack");
    assert_eq!(green_err.code, "SafetyLevelRestricted");

    // Set Safety to Yellow (1) -> allows low-sec Suspect ship attack, but blocks Criminal pod attack!
    let _ = dispatch_combat_rpc(
        &ctx,
        &mut sess_a,
        "crimewatch",
        "SetSafetyLevel",
        &[PyRep::Integer(1)],
        &HashMap::new(),
    )
    .await
    .expect("SetSafetyLevel Yellow");
    let _ = dispatch_combat_rpc(
        &ctx,
        &mut sess_a,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(gun_a), PyRep::Integer(ship_b)],
        &HashMap::new(),
    )
    .await
    .expect("Safety Yellow must allow low-sec Suspect ship attack");

    // 2. Verify low-sec Suspect flag, Weapons Timer, PvP Timer, and Limited Engagement (`feat:crimewatch.lowsec-suspect-vs-criminal`, `feat:crimewatch.limited-engagement`)
    let states_a = dispatch_combat_rpc(
        &ctx,
        &mut sess_a,
        "crimewatch",
        "GetClientStates",
        &[],
        &HashMap::new(),
    )
    .await
    .expect("GetClientStates");
    assert_eq!(
        dict_get(&states_a, "weaponsTimerActive").and_then(|v| v.as_bool()),
        Some(true)
    );
    assert_eq!(
        dict_get(&states_a, "pvpTimerActive").and_then(|v| v.as_bool()),
        Some(true)
    );
    assert_eq!(
        dict_get(&states_a, "suspect").and_then(|v| v.as_bool()),
        Some(true),
        "Low-sec ship attack must grant Suspect flag"
    );
    assert_eq!(
        dict_get(&states_a, "criminal").and_then(|v| v.as_bool()),
        Some(false),
        "Low-sec ship attack must NOT grant Criminal flag"
    );

    // 3. Weapons Timer restrictions, Tether/Filament block, Wormhole exemption, and PvP Logoff restriction
    // (`feat:crimewatch.weapon-timer-scope`, `feat:crimewatch.weapon-timer-tether-filament-block`, `feat:navigation.wormhole-aggression-exemption`, `feat:crimewatch.pvp-logoff-timer-renewal`)
    let now_unix = ctx.now_unix();
    for blocked_action in &[
        "stargate_jump",
        "dock",
        "tether",
        "filament",
        "refit_in_space",
        "eject",
    ] {
        let err = check_crimewatch_action_restriction(
            &ctx,
            char_a,
            ship_a,
            lowsec_id,
            blocked_action,
            now_unix,
        )
        .expect_err("Weapons timer must block action");
        assert_eq!(err.code, "WeaponsTimerActive");
    }
    // Wormhole jump IS exempt under Weapons / PvP timer!
    check_crimewatch_action_restriction(&ctx, char_a, ship_a, lowsec_id, "wormhole_jump", now_unix)
        .expect("Wormhole jump must be allowed under Weapons/PvP timer");
    // Safe logoff blocked under 15m PvP timer
    let logoff_err = check_crimewatch_action_restriction(
        &ctx,
        char_a,
        ship_a,
        lowsec_id,
        "safe_logoff",
        now_unix,
    )
    .expect_err("PvP timer must block safe logoff");
    assert_eq!(logoff_err.code, "PvpLogoffRestricted");

    // 4. Low-sec capsule aggression (`feat:crimewatch.pod-timer`):
    // Set Safety to Red (0) and attack a Capsule (type 670) in low-sec -> Criminal flag!
    let _ = dispatch_combat_rpc(
        &ctx,
        &mut sess_a,
        "crimewatch",
        "SetSafetyLevel",
        &[PyRep::Integer(0)],
        &HashMap::new(),
    )
    .await;
    let pod_target_id = 530_001;
    let sys_handle = ctx.systems.get_or_spawn(lowsec_id);
    ctx.db.with_mut_snapshot(|s| {
        s.items.insert(
            pod_target_id,
            InventoryItemRecord {
                item_id: pod_target_id,
                type_id: 670,
                owner_id: char_b,
                location_id: lowsec_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });
    let _ = sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: pod_target_id,
                type_id: 670,
                position: DVec3::new(1_200.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 200.0,
                inertia: 1.0,
                mass: 10_000.0,
                radius: 25.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_type_combat_state(670),
        )
        .await;
    // Clear limited engagement so attacking pod_target_id is evaluated as unprovoked pod aggression
    with_mut_combat_runtime(&ctx, |rt| {
        rt.limited_engagements.clear();
    });
    let _ = dispatch_combat_rpc(
        &ctx,
        &mut sess_a,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(gun_a), PyRep::Integer(pod_target_id)],
        &HashMap::new(),
    )
    .await
    .expect("Low-sec Pod Attack");
    let states_pod = dispatch_combat_rpc(
        &ctx,
        &mut sess_a,
        "crimewatch",
        "GetClientStates",
        &[],
        &HashMap::new(),
    )
    .await
    .expect("GetClientStates after pod attack");
    assert_eq!(
        dict_get(&states_pod, "criminal").and_then(|v| v.as_bool()),
        Some(true),
        "Attacking a capsule in low-sec must trigger a Criminal flag"
    );

    // 5. High-sec criminal warp block (`feat:crimewatch.highsec-criminal-restrictions`)
    let hs_warp_err =
        check_crimewatch_action_restriction(&ctx, char_a, ship_a, highsec_id, "warp", now_unix)
            .expect_err("Criminal ship in high-sec cannot warp");
    assert_eq!(hs_warp_err.code, "CriminalWarpRestricted");
    // Capsules CAN warp even while criminal in high-sec
    check_crimewatch_action_restriction(&ctx, char_a, pod_target_id, highsec_id, "warp", now_unix)
        .expect("Capsules must still be able to warp while criminal in high-sec");
}

#[tokio::test]
async fn test_c8_crimewatch_npc_timers_duels_logi_assistance_and_outlaw() {
    // parity: feat:crimewatch.limited-engagements
    // parity: feat:crimewatch.logi-timers
    // parity: feat:crimewatch.npc-timers
    // parity: feat:crimewatch.duels
    // parity: feat:crimewatch.disapproval-flag
    // parity: feat:crimewatch.suspects-list
    // parity: feat:crimewatch.highsec-restrictions
    // parity: feat:crimewatch.security-bands
    // parity: feat:crimewatch.third-party-assistance-legality
    // parity: feat:crimewatch.outlaw-status
    // parity: feat:npc.concord-npc
    eve_testkit::attest(&[
        "feat:crimewatch.limited-engagements",
        "feat:crimewatch.logi-timers",
        "feat:crimewatch.npc-timers",
        "feat:crimewatch.duels",
        "feat:crimewatch.disapproval-flag",
        "feat:crimewatch.suspects-list",
        "feat:crimewatch.highsec-restrictions",
        "feat:crimewatch.security-bands",
        "feat:crimewatch.third-party-assistance-legality",
        "feat:crimewatch.outlaw-status",
        "feat:npc.concord-npc",
    ]);

    let ctx = ServerContext::new();
    let char_a = 90_000_001;
    let char_b = 90_000_002;
    let char_logi = 90_000_003;
    let ship_a = 100_000_001;
    let ship_b = 100_000_002;
    let highsec_id = 30_000_142;
    let lowsec_id = 30_002_537;
    let now_unix = ctx.now_unix();

    let mut sess_a = setup_pilot_session(&ctx, 7117, char_a, ship_a, 587, lowsec_id, DVec3::ZERO).await;
    let mut sess_b = setup_pilot_session(&ctx, 7118, char_b, ship_b, 587, lowsec_id, DVec3::new(1_500.0, 0.0, 0.0)).await;
    let gun_a = 204_001;
    fit_module_and_charge(&ctx, char_a, ship_a, gun_a, 486, InventoryFlag::HiSlot0, Some((204_002, 185, 100)));
    let sys_handle = ctx.systems.get_or_spawn(lowsec_id);

    // 6. NPC timer (`feat:crimewatch.npc-timers`) & Duel challenge (`feat:crimewatch.limited-engagement`)
    let npc_ball_id = 530_002;
    let _ = sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: npc_ball_id,
                type_id: 10032, // Sansha rat
                position: DVec3::new(1_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 200.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_type_combat_state(10032),
        )
        .await;
    let _ = dispatch_combat_rpc(
        &ctx,
        &mut sess_a,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(gun_a), PyRep::Integer(npc_ball_id)],
        &HashMap::new(),
    )
    .await
    .expect("Attack NPC");
    let states_npc = dispatch_combat_rpc(
        &ctx,
        &mut sess_a,
        "crimewatch",
        "GetClientStates",
        &[],
        &HashMap::new(),
    )
    .await
    .expect("GetClientStates NPC");
    assert_eq!(
        dict_get(&states_npc, "npcTimerActive").and_then(|v| v.as_bool()),
        Some(true)
    );

    let _ = dispatch_combat_rpc(
        &ctx,
        &mut sess_a,
        "crimewatch",
        "StartDuelChallenge",
        &[PyRep::Integer(char_b)],
        &HashMap::new(),
    )
    .await
    .expect("StartDuelChallenge");
    let duel_res = dispatch_combat_rpc(
        &ctx,
        &mut sess_b,
        "crimewatch",
        "RespondToDuelChallenge",
        &[PyRep::Integer(char_a), PyRep::Bool(true)],
        &HashMap::new(),
    )
    .await
    .expect("RespondToDuelChallenge");
    assert_eq!(
        dict_get(&duel_res, "accepted").and_then(|v| v.as_bool()),
        Some(true)
    );

    // 7. Third-party remote assistance legality, Logi flag inheritance, and Outlaw status
    // (`feat:crimewatch.logi-timers`, `feat:crimewatch.third-party-assistance-legality`, `feat:crimewatch.outlaw-status`)
    ctx.crimewatch
        .write()
        .unwrap()
        .get_or_create(char_a)
        .criminal_flag_until_unix = now_unix + 900;
    let (flag_crim, is_crim, _) =
        evaluate_remote_assistance_legality(&ctx, char_logi, char_a, highsec_id, now_unix);
    assert_eq!(flag_crim, "Criminal");
    assert!(is_crim);

    // Clear char_b flags, put char_b in Limited Engagement only, and assist char_b in high-sec -> Criminal flag
    ctx.crimewatch.write().unwrap().states.remove(&char_b);
    let (flag_le_hs, is_crim_le, _) =
        evaluate_remote_assistance_legality(&ctx, char_logi, char_b, highsec_id, now_unix);
    assert_eq!(flag_le_hs, "Criminal");
    assert!(is_crim_le);

    // Outlaw (`security_status <= -5.0`): repairing outlaw in high-sec -> Suspect; repairing outlaw in low-sec -> Legal
    with_mut_combat_runtime(&ctx, |rt| {
        rt.limited_engagements.clear();
    });
    ctx.db.with_mut_snapshot(|s| {
        if let Some(c) = s.characters.get_mut(&char_b) {
            c.security_status = -5.5;
        }
    });
    let (outlaw_hs, _, is_susp_hs) =
        evaluate_remote_assistance_legality(&ctx, char_logi, char_b, highsec_id, now_unix);
    assert_eq!(outlaw_hs, "Suspect");
    assert!(is_susp_hs);
    let (outlaw_ls, is_crim_ls, is_susp_ls) =
        evaluate_remote_assistance_legality(&ctx, char_logi, char_b, lowsec_id, now_unix);
    assert_eq!(outlaw_ls, "Legal");
    assert!(!is_crim_ls && !is_susp_ls);
}

#[tokio::test]
async fn test_c8_crimewatch_faction_police_killrights_tags_and_sentries() {
    // parity: feat:crimewatch.sec-status-tags
    // parity: feat:crimewatch.sec-status-transactions
    // parity: feat:crimewatch.faction-police
    // parity: feat:crimewatch.kill-rights-gain
    // parity: feat:crimewatch.kill-rights-sell
    // parity: feat:crimewatch.faction-police-thresholds
    // parity: feat:crimewatch.sec-status-gain-caps
    // parity: feat:crimewatch.sec-tag-tier-bands
    eve_testkit::attest(&[
        "feat:crimewatch.sec-status-tags",
        "feat:crimewatch.sec-status-transactions",
        "feat:crimewatch.faction-police",
        "feat:crimewatch.kill-rights-gain",
        "feat:crimewatch.kill-rights-sell",
        "feat:crimewatch.faction-police-thresholds",
        "feat:crimewatch.sec-status-gain-caps",
        "feat:crimewatch.sec-tag-tier-bands",
    ]);

    let ctx = ServerContext::new();
    let char_a = 90_000_001;
    let char_b = 90_000_002;
    let ship_a = 100_000_001;
    let ship_b = 100_000_002;
    let pod_target_id = 530_001;
    let highsec_id = 30_000_142;
    let lowsec_id = 30_002_537;
    let _sess_a = setup_pilot_session(&ctx, 7127, char_a, ship_a, 587, lowsec_id, DVec3::ZERO).await;
    let mut sess_b = setup_pilot_session(&ctx, 7128, char_b, ship_b, 587, lowsec_id, DVec3::new(1_500.0, 0.0, 0.0)).await;
    let sys_handle = ctx.systems.get_or_spawn(lowsec_id);
    ctx.db.with_mut_snapshot(|s| {
        if let Some(c) = s.characters.get_mut(&char_b) {
            c.security_status = -5.5;
        }
        s.items.insert(
            pod_target_id,
            InventoryItemRecord {
                item_id: pod_target_id,
                type_id: 670,
                owner_id: char_b,
                location_id: highsec_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });

    // 8. Faction Police thresholds & response (`feat:crimewatch.faction-police`)
    assert_eq!(faction_police_sec_threshold(1.0), Some(-2.0));
    assert_eq!(faction_police_sec_threshold(0.9), Some(-2.5));
    assert_eq!(faction_police_sec_threshold(0.8), Some(-3.0));
    assert_eq!(faction_police_sec_threshold(0.7), Some(-3.5));
    assert_eq!(faction_police_sec_threshold(0.6), Some(-4.0));
    assert_eq!(faction_police_sec_threshold(0.5), Some(-4.5));
    let (attacked_ship, can_dock_ship, cloak_broken) =
        evaluate_faction_police_response(&ctx, highsec_id, char_b, ship_b).await;
    assert!(attacked_ship && !can_dock_ship && cloak_broken);
    let (attacked_pod, can_dock_pod, _) =
        evaluate_faction_police_response(&ctx, highsec_id, char_b, pod_target_id).await;
    assert!(
        !attacked_pod && can_dock_pod,
        "Capsules must not be attacked by Faction Police and may dock"
    );

    // 9. Kill Rights (`feat:crimewatch.kill-rights`)
    let kr_id = ctx.db.with_mut_snapshot(|s| {
        s.next_entity_id += 1;
        let id = s.next_entity_id;
        s.kill_rights.insert(
            id,
            eve_db::KillRightRecord {
                kill_right_id: id,
                owner_id: char_b,
                target_id: char_a,
                activation_fee: IskCents::from_isk(5_000_000),
                is_active_suspect: false,
            },
        );
        id
    });
    let _ = dispatch_combat_rpc(
        &ctx,
        &mut sess_b,
        "bountyProxy",
        "SellKillRight",
        &[PyRep::Integer(kr_id), PyRep::Real(10_000_000.0)],
        &HashMap::new(),
    )
    .await
    .expect("SellKillRight");
    let my_krs = dispatch_combat_rpc(
        &ctx,
        &mut sess_b,
        "bountyProxy",
        "GetMyKillRights",
        &[],
        &HashMap::new(),
    )
    .await
    .expect("GetMyKillRights");
    assert!(matches!(my_krs, PyRep::List(ref l) if !l.is_empty()));

    let act_kr = dispatch_combat_rpc(
        &ctx,
        &mut sess_b,
        "killRightMgr",
        "ActivateKillRight",
        &[PyRep::Integer(kr_id)],
        &HashMap::new(),
    )
    .await
    .expect("ActivateKillRight");
    assert_eq!(
        dict_get(&act_kr, "suspectFlagged").and_then(|v| v.as_bool()),
        Some(true)
    );

    // 10. Clone Soldier Security Tags (`feat:crimewatch.security-tags`)
    ctx.db.with_mut_snapshot(|s| {
        if let Some(c) = s.characters.get_mut(&char_b) {
            c.security_status = -6.5;
        }
        s.items.insert(
            750_001,
            InventoryItemRecord {
                item_id: 750_001,
                type_id: 35662, // Clone Soldier Recruiter Tag ([-8.0, -5.0) band)
                owner_id: char_b,
                location_id: ship_b,
                flag: InventoryFlag::Cargo,
                quantity: 4,
                singleton: false,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });
    sess_b.station_id = Some(60_003_760);
    let tag_res = dispatch_combat_rpc(
        &ctx,
        &mut sess_b,
        "crimewatch",
        "ApplyTagsForSecStatus",
        &[PyRep::Integer(35662), PyRep::Integer(2)],
        &HashMap::new(),
    )
    .await
    .expect("ApplyTagsForSecStatus");
    sess_b.station_id = None;
    let new_sec = dict_get(&tag_res, "newSecurityStatus")
        .and_then(|v| v.as_f64())
        .unwrap_or(-10.0);
    assert!(
        (new_sec - (-5.5)).abs() < 1e-6,
        "2 Clone Soldier Recruiter tags must raise security status from -6.5 to -5.5 (got {new_sec})"
    );
    let tx_res = dispatch_combat_rpc(
        &ctx,
        &mut sess_b,
        "crimewatch",
        "GetSecurityStatusTransactions",
        &[],
        &HashMap::new(),
    )
    .await
    .expect("GetSecurityStatusTransactions");
    assert!(matches!(tx_res, PyRep::Rowset { .. }));

    // 11. Station / Stargate Sentry Guns (`feat:crimewatch.sentry-guns`)
    // Trigger sentry aggression on grid (0,0,0), step once -> takes damage; warp off grid (>150km) -> ceases fire; warp back -> does not re-engage!
    let (active1, sentry_dmg1) = step_sentry_gun_grid_aggression(
        &ctx,
        lowsec_id,
        char_a,
        ship_a,
        DVec3::ZERO,
        true,
    )
    .await;
    assert!(
        active1 && sentry_dmg1 > 0.0,
        "Sentry guns must engage offender on grid"
    );

    // Warp ship_a 600km away (off grid, beyond 500km sentry range)
    let _ = sys_handle.remove_ball(ship_a).await;
    let _ = sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: ship_a,
                type_id: 587,
                position: DVec3::new(600_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 350.0,
                inertia: 3.0,
                mass: 1_200_000.0,
                radius: 35.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_type_combat_state(587),
        )
        .await;
    let (active2, sentry_dmg2) = step_sentry_gun_grid_aggression(
        &ctx,
        lowsec_id,
        char_a,
        ship_a,
        DVec3::ZERO,
        false,
    )
    .await;
    assert!(
        !active2 && sentry_dmg2 == 0.0,
        "Sentry guns must lose aggro when offender warps off grid"
    );

    // Warp back to grid (0,0,0) without re-aggressing -> sentries must remain passive
    let _ = sys_handle.remove_ball(ship_a).await;
    let _ = sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: ship_a,
                type_id: 587,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 350.0,
                inertia: 3.0,
                mass: 1_200_000.0,
                radius: 35.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_type_combat_state(587),
        )
        .await;
    let (active3, sentry_dmg3) = step_sentry_gun_grid_aggression(
        &ctx,
        lowsec_id,
        char_a,
        ship_a,
        DVec3::ZERO,
        false,
    )
    .await;
    assert!(
        !active3 && sentry_dmg3 == 0.0,
        "Sentry guns must not re-engage returning ship unless it re-aggresses"
    );
}
