//! Shared helpers for Abyssal Deadspace room combat: clears room NPCs through the production
//! in-space damage pipeline (`SystemHandle::apply_damage_with_attacker` -> ballpark removal + death hook)
//! and verifies the room's Triglavian Bioadaptive Cache and its persisted loot.

use eve_destiny::DamageVector;
use eve_server::router::ServerContext;
use eve_server::services::combat_crimewatch::{combat_npc_ai::step_extended_npc_ai_sync, with_combat_runtime};
use eve_server::services::pve_special_sites::{
    with_pve_special_reg, ABYSSAL_ROOM_NPC_TYPES, BIOADAPTIVE_CACHE_TYPE_ID,
    BIOADAPTIVE_CACHE_WRECK_TYPE_ID, TRIGLAVIAN_SURVEY_DATABASE_TYPE_ID,
};

/// Ticks Abyssal room NPC AI (verifying Triglavian room NPCs target and damage the pilot's ship and have
/// difficulty-scaled HP), then destroys every NPC of `room_idx` (0-based) through the damage pipeline.
pub async fn destroy_room_npcs_via_damage(ctx: &ServerContext, char_id: i64, room_idx: usize) {
    let (sys, ship_id, diff_mult, npcs) = with_pve_special_reg(ctx, |reg| {
        let p = reg.abyssal_pockets.get(&char_id).expect("pocket");
        (p.abyssal_solar_system_id, p.ship_id, p.npc_difficulty_mult, p.rooms[room_idx].npc_ball_ids.clone())
    });
    assert!(!npcs.is_empty(), "room {} must have NPCs", room_idx + 1);
    let h = ctx.systems.get_or_spawn(sys);
    if let Ok(Some(mut cs)) = h.get_combat_state(ship_id).await {
        if cs.max_shield_hp < 50_000.0 {
            cs.max_shield_hp = 50_000.0;
            cs.shield_hp = 50_000.0;
            let _ = h.set_combat_state(ship_id, cs).await;
        }
    }
    let ship_before = h.get_combat_state(ship_id).await.unwrap().expect("pilot ship has CombatState");
    let hp_before = ship_before.shield_hp + ship_before.armor_hp + ship_before.structure_hp;
    step_extended_npc_ai_sync(ctx, sys);
    let ship_after = h.get_combat_state(ship_id).await.unwrap().expect("pilot ship CombatState after AI tick");
    let hp_after = ship_after.shield_hp + ship_after.armor_hp + ship_after.structure_hp;
    assert!(hp_after < hp_before, "Triglavian room NPC AI tick must damage pilot ship ({hp_after} < {hp_before})");
    let _ = h.heal_combat_state(ship_id).await;
    for bid in npcs {
        let (_, ball) = h.get_ball_snapshot(bid).await.unwrap().expect("room NPC ball spawned");
        assert!(ABYSSAL_ROOM_NPC_TYPES.contains(&ball.type_id), "NPC {bid} has real Abyssal type, got {}", ball.type_id);
        let target = with_combat_runtime(ctx, |rt| rt.npc_entities.get(&bid).and_then(|n| n.target_id));
        assert_eq!(target, Some(ship_id), "NPC {bid} must target pilot ship {ship_id}");
        let cs = h.get_combat_state(bid).await.unwrap().expect("room NPC has CombatState");
        let base_cs = ctx.build_type_combat_state(ball.type_id);
        assert!((cs.max_structure_hp - base_cs.max_structure_hp * diff_mult.max(1.0)).abs() < 1e-3, "NPC {bid} HP scaled by difficulty");
        let dmg = DamageVector { em: 1.0e7, thermal: 1.0e7, kinetic: 1.0e7, explosive: 1.0e7 };
        let res = h.apply_damage_with_attacker(bid, dmg, Some(char_id)).await;
        assert!(matches!(res, Ok(Some((_, true, _)))), "NPC {bid} must be destroyed by damage: {res:?}");
        assert!(h.get_ball_snapshot(bid).await.unwrap().is_none(), "destroyed NPC {bid} removed from ballpark");
    }
    // Approach room transfer/exit gate within proximity (<= 2500m)
    let (pid, curr_room) = with_pve_special_reg(ctx, |reg| {
        let p = reg.abyssal_pockets.get(&char_id).expect("pocket");
        (p.pocket_id, p.current_room)
    });
    let gate_id = 1_895_000_000 + pid * 10 + (curr_room as i64);
    if let Ok(Some((_, gb))) = h.get_ball_snapshot(gate_id).await {
        let _ = h.set_ball_position(ship_id, gb.position + glam::DVec3::new(100.0, 0.0, 0.0)).await;
    }
}

/// Verifies the room's Bioadaptive Cache in space, destroys it via combat damage, and verifies its loot transfers to the wreck.
pub async fn assert_room_cache_loot(ctx: &ServerContext, char_id: i64, room_idx: usize, survey_qty: i32, filament_type: u32) {
    let (sys, cache_id) = with_pve_special_reg(ctx, |reg| {
        let p = reg.abyssal_pockets.get(&char_id).expect("pocket");
        (p.abyssal_solar_system_id, p.rooms[room_idx].cache_item_id)
    });
    let h = ctx.systems.get_or_spawn(sys);
    let (_, ball) = h.get_ball_snapshot(cache_id).await.unwrap().expect("Bioadaptive Cache ball in space");
    assert_eq!(ball.type_id, BIOADAPTIVE_CACHE_TYPE_ID);
    assert!(h.get_combat_state(cache_id).await.unwrap().is_some(), "cache is a destroyable entity");
    let (cache_rec, mut loot) = ctx.db.with_snapshot(|s| {
        let rec = s.items.get(&cache_id).map(|i| (i.type_id, i.location_id));
        let loot: Vec<(u32, i32)> = s.items.values().filter(|i| i.location_id == cache_id).map(|i| (i.type_id, i.quantity)).collect();
        (rec, loot)
    });
    assert_eq!(cache_rec, Some((BIOADAPTIVE_CACHE_TYPE_ID, sys as i64)), "cache persisted in snap.items");
    loot.sort_unstable();
    let mut want = vec![(TRIGLAVIAN_SURVEY_DATABASE_TYPE_ID, survey_qty), (filament_type, 1)];
    want.sort_unstable();
    assert_eq!(loot, want, "room {} cache loot", room_idx + 1);
    let dmg = DamageVector { em: 1.0e6, thermal: 1.0e6, kinetic: 1.0e6, explosive: 1.0e6 };
    let res = h.apply_damage_with_attacker(cache_id, dmg, Some(char_id)).await;
    assert!(matches!(res, Ok(Some((_, true, _)))), "Bioadaptive Cache {cache_id} destroyed by damage: {res:?}");
    let (wreck_id, mut wreck_loot) = ctx.db.with_snapshot(|s| {
        let wid = s.items.values().filter(|i| i.type_id == BIOADAPTIVE_CACHE_WRECK_TYPE_ID && i.location_id == sys as i64).max_by_key(|i| i.item_id).map(|i| i.item_id).expect("cache wreck item");
        let wl: Vec<(u32, i32)> = s.items.values().filter(|i| i.location_id == wid).map(|i| (i.type_id, i.quantity)).collect();
        (wid, wl)
    });
    assert!(h.get_ball_snapshot(wreck_id).await.unwrap().is_some(), "cache wreck ball spawned in space");
    wreck_loot.sort_unstable();
    assert_eq!(wreck_loot, want, "room {} cache loot moved into wreck {wreck_id}", room_idx + 1);
}
