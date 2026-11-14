//! Unit tests for `space_pochven_wormholes` (Pochven topology, wormhole geometry, filament candidates, static WHs, guard spawns).

#![forbid(unsafe_code)]

use super::*;
use crate::services::fleet_social::fleet_social_fleet_ops::fleet_social_fleet_ops_tests::{
    clone_char_with, test_ctx_with_clock,
};

#[test]
fn pochven_wormhole_detection_position_and_neighbor_topology() {
    let ctx = test_ctx_with_clock(1_800_000_000);
    let (border, internal, home) = pochven_topology_pools(&ctx.sde);
    assert_eq!(border.len(), 6);
    assert_eq!(internal.len(), 18);
    assert_eq!(home.len(), 3);
    for &b in &border {
        assert!(!internal.contains(&b));
        assert!(!home.contains(&b));
    }

    let poch_sys = border[0];
    let mut sig = CosmicSignatureRecord {
        signature_id: "PCH-101".into(),
        solar_system_id: 30_000_142,
        sig_group: "Wormhole".into(),
        type_name: "Wormhole".into(),
        position: [1_000.0, 2_000.0, 3_000.0],
        signal_strength: 1.0,
        wormhole_type_id: Some(26_272),
        destination_system_id: Some(30_000_144),
        created_unix: 100,
        lifetime_seconds: 3600,
        max_mass_kg: 1e9,
        remaining_mass_kg: 1e9,
        max_jump_mass_kg: 1e8,
        collapsed: false,
    };
    assert!(!is_pochven_wormhole(&ctx.sde, &sig));
    assert_eq!(
        pochven_wormhole_ball_position(&ctx.sde, &sig),
        DVec3::new(1_000.0, 2_000.0, 3_000.0)
    );

    // Pochven wormhole type ID -> true and +30km X offset
    sig.wormhole_type_id = Some(56_026);
    assert!(is_pochven_wormhole(&ctx.sde, &sig));
    assert_eq!(
        pochven_wormhole_ball_position(&ctx.sde, &sig),
        DVec3::new(31_000.0, 2_000.0, 3_000.0)
    );

    // Pochven destination system -> true even with generic K162 type
    sig.wormhole_type_id = Some(26_272);
    sig.destination_system_id = Some(poch_sys);
    assert!(is_pochven_wormhole(&ctx.sde, &sig));

    // Neighbors within 5 LY of a Pochven system
    let k_neighbors = pochven_kspace_neighbors_within_ly(&ctx.sde, poch_sys, 5.0);
    assert!(!k_neighbors.is_empty());
    for &kn in &k_neighbors {
        assert!(kn < 31_000_000 && kn != 30_100_000);
        assert_ne!(ctx.sde.solar_systems[&kn].region_id, POCHVEN_REGION_ID);
    }

    // Reverse lookup from a K-space neighbor finds a Pochven system
    let rev = nearest_pochven_system_within_ly(&ctx.sde, k_neighbors[0], 5.0, 0)
        .expect("K-space neighbor within 5 LY of Pochven");
    assert_eq!(ctx.sde.solar_systems[&rev].region_id, POCHVEN_REGION_ID);

    // Invalid inputs return empty / None
    assert!(pochven_kspace_neighbors_within_ly(&ctx.sde, 99_999_999, 5.0).is_empty());
    assert_eq!(nearest_pochven_system_within_ly(&ctx.sde, poch_sys, 5.0, 0), None);
    assert_eq!(nearest_pochven_system_within_ly(&ctx.sde, 30_100_000, 5.0, 0), None);
    assert_eq!(nearest_pochven_system_within_ly(&ctx.sde, 31_000_005, 5.0, 0), None);
    assert_eq!(nearest_pochven_system_within_ly(&ctx.sde, k_neighbors[0], 0.0, 0), None);
}

#[test]
fn pochven_filament_candidates_and_static_wormholes() {
    let ctx = test_ctx_with_clock(1_800_000_000);
    let (border, internal, home) = pochven_topology_pools(&ctx.sde);
    let poch_sys = border[0];
    let char_id = 910_201_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let ch = clone_char_with(snap, char_id, "Trig Traveler", 98_000_001, poch_sys, 100_000_001);
        snap.characters.insert(char_id, ch);
    });

    // Without >= 6.0 Triglavian standing, Home-5/15 (56080..=56081) fails
    assert_eq!(
        resolve_pochven_filament_candidates(&ctx, 56_080, 30_000_142, char_id)
            .unwrap_err()
            .code,
        "SystemCheck_JumpFailed_StandingsTooLow"
    );

    // Krai filaments (56070, 56072, 56074) exclude home systems when standing < 6.0
    for ftid in [56_070, 56_072, 56_074] {
        let cands = resolve_pochven_filament_candidates(&ctx, ftid, 30_000_142, char_id).unwrap();
        assert_eq!(cands.len(), 8);
        assert!(cands.iter().all(|s| !home.contains(s)));
    }

    // Border, Internal, Extraction, Devana, and fallback
    assert_eq!(
        resolve_pochven_filament_candidates(&ctx, 56_076, 30_000_142, char_id).unwrap(),
        border
    );
    assert_eq!(
        resolve_pochven_filament_candidates(&ctx, 56_078, 30_000_142, char_id).unwrap(),
        internal
    );
    assert!(!resolve_pochven_filament_candidates(&ctx, 56_065, poch_sys, char_id).unwrap().is_empty());
    assert!(!resolve_pochven_filament_candidates(&ctx, 56_068, poch_sys, char_id).unwrap().is_empty());
    assert_eq!(
        resolve_pochven_filament_candidates(&ctx, 99_999, 30_000_142, char_id).unwrap(),
        border
    );

    // With Triglavian standing >= 6.0, Home filament returns the 3 home systems and Krai has 9
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().standings.insert(500_026, 7.0);
    });
    assert_eq!(
        resolve_pochven_filament_candidates(&ctx, 56_080, 30_000_142, char_id).unwrap(),
        home
    );
    assert_eq!(
        resolve_pochven_filament_candidates(&ctx, 56_070, 30_000_142, char_id).unwrap().len(),
        9
    );

    // Static wormholes in Pochven system produce C729 and R081
    let statics = pochven_static_wormholes(&ctx.sde, poch_sys, 1, 1_800_000_000);
    assert_eq!(statics.len(), 2);
    assert_eq!(statics[0].wormhole_type_id, Some(56_026));
    assert_eq!(statics[1].wormhole_type_id, Some(56_541));
}

#[tokio::test]
async fn open_pochven_wormhole_spawns_k162_exit_and_side_guards() {
    let ctx = test_ctx_with_clock(1_800_000_000);
    let (border, _, _) = pochven_topology_pools(&ctx.sde);
    let poch_sys = border[0];
    let dest_sys = 30_000_142_u32;
    let statics = pochven_static_wormholes(&ctx.sde, poch_sys, 0, 1_800_000_000);
    let mut sig = statics[0].clone();
    sig.destination_system_id = Some(dest_sys);

    let (wh_pos, guards_poch) =
        open_pochven_wormhole_and_spawn_guards(&ctx, poch_sys, &sig, 880_001).await;
    assert_eq!(guards_poch.len(), 6);
    assert!((wh_pos.x - (sig.position[0] + 30_000.0)).abs() < 1e-3);

    // Destination K-space system now has K162 signature
    ctx.db.with_snapshot(|snap| {
        let list = snap.cosmic_signatures.get(&dest_sys).expect("dest sigs");
        assert!(list.iter().any(|s| s.signature_id == format!("K162-{}", sig.signature_id)));
    });

    // K-space side spawns 2 Triglavian guards
    let k_guards = ensure_pochven_side_guards(&ctx, dest_sys, 890_001, DVec3::ZERO, false).await;
    assert_eq!(k_guards.len(), 2);
}

#[tokio::test]
async fn r11_k12_collapsed_pochven_wormhole_skips_spawn_and_guards_have_slim_meta() {
    let ctx = test_ctx_with_clock(1_800_000_000);
    let (border, _, _) = pochven_topology_pools(&ctx.sde);
    let poch_sys = border[0];
    let dest_sys = 30_000_142_u32;
    let statics = pochven_static_wormholes(&ctx.sde, poch_sys, 0, 1_800_000_000);
    let mut collapsed_sig = statics[0].clone();
    collapsed_sig.destination_system_id = Some(dest_sys);
    collapsed_sig.collapsed = true;

    let wh_item_id = 889_100_i64;
    let (_wh_pos, no_guards) =
        open_pochven_wormhole_and_spawn_guards(&ctx, poch_sys, &collapsed_sig, wh_item_id).await;
    assert!(no_guards.is_empty());
    let poch_handle = ctx.systems.get_or_spawn(poch_sys);
    assert!(poch_handle.get_ball_snapshot(wh_item_id).await.ok().flatten().is_none());
    ctx.db.with_snapshot(|snap| {
        let k162_id = format!("K162-{}", collapsed_sig.signature_id);
        let has_k162 = snap
            .cosmic_signatures
            .get(&dest_sys)
            .is_some_and(|list| list.iter().any(|s| s.signature_id == k162_id));
        assert!(!has_k162);
    });

    let guards = ensure_pochven_side_guards(&ctx, poch_sys, 889_200, DVec3::ZERO, true).await;
    assert_eq!(guards.len(), 6);
    let guard_entities: Vec<(i64, u32)> = {
        let rt = ctx.combat_ephemeral.read().unwrap_or_else(|e| e.into_inner());
        guards
            .iter()
            .map(|&gid| {
                let ent = rt.npc_entities.get(&gid).expect("guard entity");
                assert_eq!(ent.spawn_group_id, 0);
                (gid, ent.faction_id)
            })
            .collect()
    };
    {
        let bp = poch_handle.ballpark.read().unwrap_or_else(|e| e.into_inner());
        for (gid, faction_id) in guard_entities {
            let meta = bp.ball_slim_meta.get(&gid).expect("guard slim meta");
            assert_eq!(meta.owner_id, faction_id as i64);
            assert_eq!(
                meta.corp_id,
                crate::services::combat_crimewatch::combat_npc_ai::resolve_npc_corp_id_for_sde_faction(
                    &ctx.sde,
                    meta.owner_id,
                )
            );
            assert_eq!(meta.category_id, 11);
            assert!(meta.name.is_some());
        }
    }
}
