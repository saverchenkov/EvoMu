use super::combat_weapons_spool_tests::{make_fighter_item, make_spool_test_ctx, spawn_spool_ball, InventoryFlag};
use super::*;
use glam::DVec3;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn handle_destruction_non_ship_npc_bounty_and_battleship_with_escape_bay() {
    let (ctx, _, attacker_id, attacker_ship, sys_id) = make_spool_test_ctx();
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let now_unix = ctx.now_unix();

    // 1. Non-ship object returns zeroed tuple immediately
    let non_ship = 660_010_001i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            non_ship,
            make_fighter_item(non_ship, 2_456, attacker_id, sys_id as i64, InventoryFlag::None, 1),
        );
    });
    let non_ship_ball = spawn_spool_ball(&ctx, sys_id, non_ship, 2_456, DVec3::ZERO, 100.0, 0.0, 100.0, 0.0).await;
    let res_ns = handle_ship_or_npc_destruction(
        &ctx,
        &sys_handle,
        attacker_id,
        attacker_ship,
        47_271,
        47_240,
        non_ship,
        &non_ship_ball,
        None,
        sys_id,
        150.0,
        now_unix,
    )
    .await;
    assert_eq!(res_ns.0, 0);

    // 2. NPC pirate destruction credits pending bounty, updates sec status, and records killmail
    let npc_id = 1_895_000_123i64;
    let npc_ball = spawn_spool_ball(&ctx, sys_id, npc_id, 500_001, DVec3::new(1_000.0, 0.0, 0.0), 0.0, 0.0, 10.0, 0.0).await;
    let before_sec = ctx.db.with_snapshot(|s| s.characters.get(&attacker_id).map(|c| c.security_status).unwrap());
    let (npc_km, npc_wreck, _, npc_hash) = handle_ship_or_npc_destruction(
        &ctx,
        &sys_handle,
        attacker_id,
        attacker_ship,
        47_271,
        47_240,
        npc_id,
        &npc_ball,
        None,
        sys_id,
        1_250.0,
        now_unix,
    )
    .await;
    assert!(npc_km > 0 && npc_wreck > 0 && !npc_hash.is_empty());
    let after_sec = ctx.db.with_snapshot(|s| s.characters.get(&attacker_id).map(|c| c.security_status).unwrap());
    assert!(after_sec >= before_sec);

    // 3. Player Battleship (24690) with Frigate Escape Bay (11393), fitted module, gift wrap (3468), and courier contract
    let victim_id = 90_000_002i64;
    let victim_bs = 200_000_777i64;
    let esc_frig = 200_000_778i64;
    let cargo_in_bs = 750_000_001i64;
    let wrap_in_bs = 750_000_002i64;
    let wrapped_item = 750_000_003i64;
    let hi_mod = 750_000_004i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(victim_bs, make_fighter_item(victim_bs, 24_690, victim_id, sys_id as i64, InventoryFlag::None, 1));
        snap.items.insert(esc_frig, make_fighter_item(esc_frig, 11_393, victim_id, victim_bs, InventoryFlag::from_u16(177), 1));
        snap.items.insert(cargo_in_bs, make_fighter_item(cargo_in_bs, 587, victim_id, victim_bs, InventoryFlag::Cargo, 1));
        snap.items.insert(wrap_in_bs, make_fighter_item(wrap_in_bs, 3_468, victim_id, victim_bs, InventoryFlag::Cargo, 1));
        snap.items.insert(wrapped_item, make_fighter_item(wrapped_item, 587, victim_id, wrap_in_bs, InventoryFlag::Cargo, 1));
        snap.items.insert(hi_mod, make_fighter_item(hi_mod, 47_240, victim_id, victim_bs, InventoryFlag::HiSlot0, 1));
        snap.contracts.insert(
            99_777,
            eve_db::ContractRecord {
                contract_id: 99_777,
                issuer_id: victim_id,
                acceptor_id: Some(victim_id),
                contract_type: "Courier".into(),
                status: "InProgress".into(),
                start_station_id: 60_003_760,
                end_station_id: 60_003_761,
                reward: eve_db::IskCents(50_000_000),
                collateral: eve_db::IskCents(100_000_000),
                item_ids: vec![cargo_in_bs],
                expiration_unix: now_unix + 86_400,
                assignee_id: victim_id,
                date_issued_unix: now_unix,
                title: "BS Courier".into(),
                ..Default::default()
            },
        );
        if let Some(ch) = snap.characters.get_mut(&victim_id) {
            ch.active_ship_id = victim_bs;
            ch.solar_system_id = sys_id;
        }
    });
    let bs_ball = spawn_spool_ball(&ctx, sys_id, victim_bs, 24_690, DVec3::new(3_000.0, 0.0, 0.0), 10.0, 10.0, 10.0, 100.0).await;
    let (bs_km, bs_wreck, bs_corpse, _) = handle_ship_or_npc_destruction(
        &ctx,
        &sys_handle,
        attacker_id,
        attacker_ship,
        47_271,
        47_240,
        victim_bs,
        &bs_ball,
        Some((victim_id, victim_bs)),
        sys_id,
        9_500.0,
        now_unix,
    )
    .await;
    assert!(bs_km > 0 && bs_wreck > 0 && bs_corpse == 0);
    // Victim now pilots escape frigate (esc_frig) & courier contract is Failed
    let (active_after_bs, bs_contract_status) = ctx.db.with_snapshot(|s| {
        (
            s.characters.get(&victim_id).map(|c| c.active_ship_id),
            s.contracts.get(&99_777).map(|c| c.status.clone()),
        )
    });
    assert_eq!(active_after_bs, Some(esc_frig));
    assert_eq!(bs_contract_status.as_deref(), Some("Failed"));

    // 4. Capsule pod (670) destruction destroys implants, spawns corpse (type 25), and respawns pod at home station
    let pod_id = 200_000_888i64;
    let imp_item = 750_000_010i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(pod_id, make_fighter_item(pod_id, 670, victim_id, sys_id as i64, InventoryFlag::None, 1));
        snap.items.insert(imp_item, make_fighter_item(imp_item, 9_941, victim_id, victim_id, InventoryFlag::Implant, 1));
        snap.active_implants.insert(victim_id, vec![9_942]);
        if let Some(ch) = snap.characters.get_mut(&victim_id) {
            ch.active_ship_id = pod_id;
            ch.solar_system_id = sys_id;
        }
    });
    let pod_ball = spawn_spool_ball(&ctx, sys_id, pod_id, 670, DVec3::new(4_000.0, 0.0, 0.0), 0.0, 0.0, 5.0, 0.0).await;
    let (pod_km, pod_wreck, pod_corpse, _) = handle_ship_or_npc_destruction(
        &ctx,
        &sys_handle,
        attacker_id,
        attacker_ship,
        47_271,
        47_240,
        pod_id,
        &pod_ball,
        Some((victim_id, pod_id)),
        sys_id,
        800.0,
        now_unix,
    )
    .await;
    assert!(pod_km > 0 && pod_wreck == 0 && pod_corpse > 0);
    let (corpse_exists, imps_cleared) = ctx.db.with_snapshot(|s| {
        (
            s.items.get(&pod_corpse).is_some_and(|i| i.type_id == 25),
            !s.items.contains_key(&imp_item),
        )
    });
    assert!(corpse_exists && imps_cleared);
}
