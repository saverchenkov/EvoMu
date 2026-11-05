//! Integration tests for NPC Navy, Zarzakh gate guns, Pochven Sleepers, and NPC Mining/Haulers.

#![forbid(unsafe_code)]

use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;

async fn login_session(ctx: &ServerContext, sess_id: u64, char_id: i64) -> SessionState {
    let kw = HashMap::new();
    let mut sess = SessionState::new(sess_id);
    ctx.dispatch_rpc(
        &mut sess,
        "authentication",
        None,
        "Login",
        &[PyRep::String("demo".into()), PyRep::String("demo-secret".into())],
        &kw,
        None,
    )
    .await
    .expect("Login");
    
    // Simulate binding the character
    sess.character_id = Some(char_id);
    sess.corp_id = Some(1_000_001);
    sess
}

// parity: feat:npc.navy-npc, feat:npc.zarzakh-gate-guns-patrols, feat:npc.pochven-hypnosian-sleepers, feat:npc.npc-hauler, feat:npc.mining-op-npc
#[tokio::test]
async fn test_npc_navy_zarzakh_sleepers_haulers_live() {
    let ctx = ServerContext::new();
    
    // 1. feat:npc.pochven-hypnosian-sleepers
    let sig = eve_db::CosmicSignatureRecord {
        signature_id: "PCH-123".into(),
        solar_system_id: 30000142,
        sig_group: "Wormhole".into(),
        type_name: "Wormhole".into(),
        position: [0.0, 0.0, 0.0],
        signal_strength: 0.0,
        wormhole_type_id: Some(56_026),
        destination_system_id: Some(30100000),
        created_unix: 0,
        lifetime_seconds: 43200,
        max_mass_kg: 1e9,
        remaining_mass_kg: 1e9,
        max_jump_mass_kg: 3e8,
        collapsed: false,
    };
    
    // Find a Pochven system
    let pochven_sys_id = ctx.sde.solar_systems.values().find(|s| s.region_id == 10_000_070).map(|s| s.solar_system_id).unwrap_or(30045339);
    
    let (_, guard_ids) = eve_server::services::space_pochven_wormholes::open_pochven_wormhole_and_spawn_guards(
        &ctx,
        pochven_sys_id,
        &sig,
        900_000_000,
    ).await;
    
    let sys_h = ctx.systems.get_or_spawn(pochven_sys_id);
    let mut has_faded = false;
    let mut has_awakened = false;
    let mut has_aroused = false;
    for gid in guard_ids {
        if let Some((_, b)) = sys_h.get_ball_snapshot(gid).await.ok().flatten() {
            if b.type_id == 56_337 { has_faded = true; }
            if b.type_id == 56_328 { has_awakened = true; }
            if b.type_id == 56_319 { has_aroused = true; }
        }
    }
    assert!(has_faded, "Expected Faded Hypnosian Sleeper");
    assert!(has_awakened, "Expected Awakened/Standard Hypnosian Sleeper");
    assert!(has_aroused, "Expected Aroused Hypnosian Sleeper");

    // 2. feat:npc.mining-op-npc and feat:npc.npc-hauler
    let jita_id = 30_000_142;
    let mining_convoy_id = ctx.living_world.ensure_system_mining_op(jita_id, &ctx.sde);
    let hauler_convoy_id = ctx.living_world.ensure_system_hauler_convoy(jita_id, &ctx.sde);
    
    let convoys = ctx.living_world.convoys.read().unwrap();
    let min_c = convoys.get(&mining_convoy_id).expect("Mining Op");
    // Jita is Caldari space, min hauler is 648 (Badger), miner is 21654
    assert_eq!(min_c.ship_type_id, 21654, "Authentic Caldari mining hull");
    
    let h_c = convoys.get(&hauler_convoy_id).expect("Hauler Convoy");
    assert_eq!(h_c.ship_type_id, 648, "Authentic Caldari industrial hauler");
    drop(convoys);

    // Test Amarr and Minmatar hauler types too for completion
    // Amarr system: Amarr (30002187)
    let amarr_hauler_convoy_id = ctx.living_world.ensure_system_hauler_convoy(30002187, &ctx.sde);
    let convoys = ctx.living_world.convoys.read().unwrap();
    let ah_c = convoys.get(&amarr_hauler_convoy_id).expect("Amarr Hauler");
    assert_eq!(ah_c.ship_type_id, 649, "Authentic Amarr industrial hauler");
    drop(convoys);

    // 3. feat:npc.zarzakh-gate-guns-patrols
    // Instead of using with_mut_combat_runtime, we can verify disapproval flags aren't used
    // and standard crimewatch penalties apply in Zarzakh by manually calling the aggression hook
    // which triggers the crimewatch timer updates.
    let attacker_char_id = 90_000_001;
    let victim_char_id = 90_000_002;
    let zarzakh_id = 30100000;
    
    // First, simulate a new PVP aggression in Zarzakh
    eve_server::services::crimewatch_timers::record_illegal_pvp_penalties_if_new(
        &ctx, attacker_char_id, victim_char_id, zarzakh_id, -1.0, false, true, ctx.now_unix(),
    );
    
    // Assert no disapproval flags are set for Zarzakh (the remediation goal)
    // The combat runtime shouldn't have disapproval flags inserted.
    let has_disapproval = eve_server::services::combat_crimewatch::with_combat_runtime(&ctx, |rt| {
        rt.disapproval_flags.contains_key(&attacker_char_id)
    });
    assert!(!has_disapproval, "Zarzakh should not use disapproval flags");

    // 4. feat:npc.navy-npc
    // Faction Navy spawns on < -2.0 standing or militia enemy. 
    // AI target selection correctly verifies standings and is_criminal.
    // We can directly call `derive_npc_archetype` to ensure Navy NPCs are resolved properly,
    // or test the standing boundaries implicitly if possible.
    let navy_npc = eve_server::services::combat_crimewatch::combat_npc_ai::derive_npc_archetype(&ctx, 29707); // Caldari Navy
    assert_eq!(navy_npc, "Navy", "Navy archetype correctly derived");
}
