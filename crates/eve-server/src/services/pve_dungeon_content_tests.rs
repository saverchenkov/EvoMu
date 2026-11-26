//! Unit tests for `pve_dungeon_content` (`schema_version = 2` parser, SDE stand-in resolution,
//! multi-room runtime instantiation, and per-`WorldRepository` catalog isolation for `#1583`).

use super::*;
use crate::router::ServerContext;
use crate::services::pve_dungeon_runtime::{
    materialize_dungeon_room_entities, on_dungeon_item_looted, on_dungeon_npc_killed,
    on_dungeon_room_entered, read_dungeon_runtime_store, resolve_deadspace_or_escalation_warp_target,
    DungeonTriggerAction, DungeonTriggerKind,
};
use crate::services::pve_dungeon_runtime_rpc::{
    resolve_rooms_and_objectives_with_content_fallback, resolve_rooms_with_content_fallback,
    spawn_dungeon_instance, spawn_sde_dungeon_runtime_instance, try_activate_dungeon_runtime_gate,
};
use eve_db::clock::VirtualServerClock;
use eve_db::WorldRepository;
use eve_sde::{DungeonTemplate, NpcSpawnGroupInfo, SdeRegistry};
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn make_ctx(sde: Arc<SdeRegistry>) -> Arc<ServerContext> {
    let db = Arc::new(WorldRepository::new_empty());
    let ctx = Arc::new(ServerContext::with_sde_and_repository(sde, db));
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_776_400_000)));
    ctx
}

fn pick_concrete_combat_npc_tid(sde: &SdeRegistry) -> u32 {
    sde.types
        .iter()
        .filter(|(&tid, t)| t.category_id == 11 && is_valid_combat_npc_type(sde, tid))
        .map(|(&tid, _)| tid)
        .min()
        .unwrap_or(23_335)
}

fn minimal_v2_catalog_json(
    dungeon_id: u32,
    entry_id: &str,
    base_title: &str,
    concrete_npc_tid: u32,
) -> String {
    format!(
        r#"{{
  "header": {{
    "schema_version": 2,
    "source": "EVE University Wiki (unit-test fixture)",
    "license": "CC BY-SA 4.0"
  }},
  "indexes": {{
    "by_dungeon_id": {{
      "{dungeon_id}": ["{entry_id}"]
    }}
  }},
  "entries": [
    {{
      "entry_id": "{entry_id}",
      "page_title": "{base_title}",
      "base_title": "{base_title}",
      "unit_index": 0,
      "unit_kind": "deadspace",
      "factions": ["Guristas Pirates"],
      "levels": [3],
      "sde_matches": {{
        "dungeon_ids": [{dungeon_id}],
        "mission_ids": []
      }},
      "rooms": [
        {{
          "room_index": 1,
          "room_name": "Outer Pocket",
          "has_acceleration_gate": true,
          "waves": [
            {{
              "wave_index": 1,
              "wave_name": "Initial Defenders",
              "timing": "initial_spawn",
              "distance_m": {{ "min_m": 2500, "max_m": 2500 }},
              "spawns": [
                {{
                  "raw_class": "frigate",
                  "raw_name": "Pithi Invader",
                  "count": {{ "min": 2, "max": 2 }},
                  "resolution": {{
                    "status": "resolved",
                    "type_ids": [{concrete_npc_tid}],
                    "candidate_type_ids": [],
                    "had_ellipsis": false
                  }}
                }},
                {{
                  "raw_class": "container",
                  "raw_name": "Guristas Munitions Cache",
                  "count": {{ "min": 1, "max": 1 }},
                  "resolution": {{
                    "status": "resolved",
                    "type_ids": [23],
                    "candidate_type_ids": [],
                    "had_ellipsis": false
                  }},
                  "loot": {{ "items": ["Antimatter Charge S"] }}
                }}
              ]
            }},
            {{
              "wave_index": 2,
              "wave_name": "Commander Reinforcement",
              "timing": "sequential_wave",
              "wave_trigger": {{
                "kind": "wave_cleared",
                "source_wave_index": 1,
                "unlocks_gate": true
              }},
              "spawns": [
                {{
                  "raw_class": "battleship",
                  "raw_name": "Unresolved Guristas Commander",
                  "count": {{ "min": 1, "max": 1 }},
                  "resolution": {{
                    "status": "unresolved",
                    "type_ids": [],
                    "candidate_type_ids": [],
                    "had_ellipsis": false
                  }}
                }},
                {{
                  "raw_class": "unmapped_weird_tag",
                  "raw_name": "Ghost Anomaly Marker",
                  "count": {{ "min": 1, "max": 1 }},
                  "resolution": {{
                    "status": "unresolved",
                    "type_ids": [],
                    "candidate_type_ids": [],
                    "had_ellipsis": false
                  }}
                }}
              ]
            }}
          ]
        }},
        {{
          "room_index": 2,
          "room_name": "Inner Bastion",
          "has_acceleration_gate": true,
          "waves": [
            {{
              "wave_index": 1,
              "wave_name": "Inner Approach Guard",
              "timing": "sequential_wave",
              "distance_m": {{ "min_m": 3200, "max_m": 3200 }},
              "wave_trigger": {{
                "kind": "ship_approached",
                "unlocks_gate": false
              }},
              "spawns": [
                {{
                  "raw_class": "cruiser",
                  "raw_name": "Pithum Abolisher",
                  "count": {{ "min": 1, "max": 1 }},
                  "resolution": {{
                    "status": "resolved",
                    "type_ids": [{concrete_npc_tid}],
                    "candidate_type_ids": [],
                    "had_ellipsis": false
                  }}
                }}
              ]
            }}
          ]
        }},
        {{
          "room_index": 3,
          "room_name": "Overseer Vault",
          "has_acceleration_gate": false,
          "waves": [
            {{
              "wave_index": 1,
              "wave_name": "Final Boss",
              "timing": "initial_spawn",
              "wave_trigger": {{
                "kind": "wave_cleared",
                "unlocks_gate": true
              }},
              "spawns": [
                {{
                  "raw_class": "frigate",
                  "raw_name": "Dread Guristas Overseer",
                  "count": {{ "min": 1, "max": 1 }},
                  "resolution": {{
                    "status": "resolved",
                    "type_ids": [{concrete_npc_tid}],
                    "candidate_type_ids": [],
                    "had_ellipsis": false
                  }}
                }}
              ]
            }}
          ]
        }}
      ]
    }}
  ]
}}"#
    )
}

#[test]
fn wiki_pve_json_v2_parser_rejects_invalid_schemas_and_malformed_entries() {
    let bad_version = r#"{
        "header": { "schema_version": 1, "source": "test", "license": "CC BY-SA 4.0" },
        "entries": []
    }"#;
    assert!(parse_and_validate_wiki_pve_json(bad_version)
        .unwrap_err()
        .contains("schema_version"));

    let dup_entry = r#"{
        "header": { "schema_version": 2, "source": "test", "license": "CC BY-SA 4.0" },
        "entries": [
            { "entry_id": "e1", "rooms": [] },
            { "entry_id": "e1", "rooms": [] }
        ]
    }"#;
    assert!(parse_and_validate_wiki_pve_json(dup_entry)
        .unwrap_err()
        .contains("duplicate wiki entry_id"));

    let bad_room_idx = r#"{
        "header": { "schema_version": 2, "source": "test", "license": "CC BY-SA 4.0" },
        "entries": [{
            "entry_id": "e1",
            "rooms": [{ "room_index": 0, "waves": [] }]
        }]
    }"#;
    assert!(parse_and_validate_wiki_pve_json(bad_room_idx)
        .unwrap_err()
        .contains("room_index must be >= 1"));

    let bad_timing = r#"{
        "header": { "schema_version": 2, "source": "test", "license": "CC BY-SA 4.0" },
        "entries": [{
            "entry_id": "e1",
            "rooms": [{
                "room_index": 1,
                "waves": [{ "wave_index": 1, "timing": "bogus_timing", "spawns": [] }]
            }]
        }]
    }"#;
    assert!(parse_and_validate_wiki_pve_json(bad_timing)
        .unwrap_err()
        .contains("unknown timing"));

    let bad_counts = r#"{
        "header": { "schema_version": 2, "source": "test", "license": "CC BY-SA 4.0" },
        "entries": [{
            "entry_id": "e1",
            "rooms": [{
                "room_index": 1,
                "waves": [{
                    "wave_index": 1,
                    "timing": "initial_spawn",
                    "spawns": [{ "raw_class": "frigate", "count": { "min": 4, "max": 1 } }]
                }]
            }]
        }]
    }"#;
    assert!(parse_and_validate_wiki_pve_json(bad_counts)
        .unwrap_err()
        .contains("min count 4 > max 1"));

    let bad_trig_time = r#"{
        "header": { "schema_version": 2, "source": "test", "license": "CC BY-SA 4.0" },
        "entries": [{
            "entry_id": "e1",
            "rooms": [{
                "room_index": 1,
                "waves": [{
                    "wave_index": 1,
                    "timing": "initial_spawn",
                    "wave_trigger": { "time_seconds": -5.0 },
                    "spawns": []
                }]
            }]
        }]
    }"#;
    assert!(parse_and_validate_wiki_pve_json(bad_trig_time)
        .unwrap_err()
        .contains("invalid trigger time_seconds"));

    let dup_wave = r#"{
        "header": { "schema_version": 2, "source": "test", "license": "CC BY-SA 4.0" },
        "entries": [{
            "entry_id": "e1",
            "rooms": [{
                "room_index": 1,
                "waves": [
                    { "wave_index": 1, "timing": "initial_spawn", "spawns": [] },
                    { "wave_index": 1, "timing": "sequential_wave", "spawns": [] }
                ]
            }]
        }]
    }"#;
    assert!(parse_and_validate_wiki_pve_json(dup_wave)
        .unwrap_err()
        .contains("duplicate wave_index"));

    let inverted_dist = r#"{
        "header": { "schema_version": 2, "source": "test", "license": "CC BY-SA 4.0" },
        "entries": [{
            "entry_id": "e1",
            "rooms": [{
                "room_index": 1,
                "waves": [{
                    "wave_index": 1,
                    "timing": "initial_spawn",
                    "distance_m": { "min_m": 80000, "max_m": 50000 },
                    "spawns": []
                }]
            }]
        }]
    }"#;
    assert!(parse_and_validate_wiki_pve_json(inverted_dist)
        .unwrap_err()
        .contains("invalid distance_m"));

    let negative_dist = r#"{
        "header": { "schema_version": 2, "source": "test", "license": "CC BY-SA 4.0" },
        "entries": [{
            "entry_id": "e1",
            "rooms": [{
                "room_index": 1,
                "waves": [{
                    "wave_index": 1,
                    "timing": "initial_spawn",
                    "distance_m": { "min_m": -100, "max_m": 100 },
                    "spawns": []
                }]
            }]
        }]
    }"#;
    assert!(parse_and_validate_wiki_pve_json(negative_dist)
        .unwrap_err()
        .contains("invalid distance_m"));

    let non_numeric_dist = r#"{
        "header": { "schema_version": 2, "source": "test", "license": "CC BY-SA 4.0" },
        "entries": [{
            "entry_id": "e1",
            "rooms": [{
                "room_index": 1,
                "waves": [{
                    "wave_index": 1,
                    "timing": "initial_spawn",
                    "distance_m": "far",
                    "spawns": []
                }]
            }]
        }]
    }"#;
    assert!(parse_and_validate_wiki_pve_json(non_numeric_dist).is_err());

    let bad_min_gt_max = r#"{
        "header": {"schema_version": 2, "source": "eve-wiki", "license": "CC BY-SA 4.0"},
        "entries": [{
            "entry_id": "bad_bounds",
            "base_title": "Bad",
            "rooms": [{
                "room_index": 1,
                "waves": [{
                    "wave_index": 1,
                    "timing": "initial_spawn",
                    "spawns": [{
                        "raw_class": "Frigate",
                        "count": {"min": 3, "max": 1}
                    }]
                }]
            }]
        }]
    }"#;
    assert!(
        parse_and_validate_wiki_pve_json(bad_min_gt_max).is_err(),
        "explicit min > max must still be rejected"
    );
}

#[test]
fn wiki_pve_catalog_per_repository_isolation_and_sde_standin_instantiation() {
    let mut sde = (*shared_sde()).clone();
    // Ensure a concrete Guristas combat NPC and battleship spawn group exist for stand-in matching.
    let concrete_npc_tid = pick_concrete_combat_npc_tid(&sde);
    let custom_did = 9_901_495_u32;
    sde.dungeons.insert(
        custom_did,
        DungeonTemplate {
            dungeon_id: custom_did,
            name: "Guristas Scout Outpost".into(),
            archetype_id: Some(8),
            faction_id: Some(500_010),
            difficulty: Some(3),
        },
    );
    sde.npc_spawn_groups.insert(
        9_901_496,
        NpcSpawnGroupInfo {
            spawn_group_id: 9_901_496,
            faction_id: 500_010,
            ship_class: 3,
            group_id: 760,
            group_name: "Guristas Battleship".into(),
            npc_type_ids: vec![concrete_npc_tid],
        },
    );
    let sde = Arc::new(sde);

    let ctx_a = make_ctx(Arc::clone(&sde));
    let ctx_b = make_ctx(Arc::clone(&sde));

    let cat_a = Arc::new(
        parse_and_validate_wiki_pve_json(&minimal_v2_catalog_json(
            custom_did,
            "guristas_scout_outpost_0",
            "Guristas Scout Outpost",
            concrete_npc_tid,
        ))
        .expect("valid v2 fixture"),
    );
    assert_eq!(cat_a.stats.schema_version, 2);
    assert_eq!(cat_a.stats.entries_total, 1);
    assert_eq!(cat_a.stats.authored_dungeon_entries, 1);
    assert_eq!(cat_a.stats.total_rooms, 3);
    assert_eq!(cat_a.stats.total_spawns, 6);

    // #1583: Test overrides live on ctx.db transient runtime slots and never leak across contexts.
    disable_wiki_pve_catalog_for_test(&ctx_b);
    install_wiki_pve_catalog_for_test(&ctx_a, Arc::clone(&cat_a));
    assert!(get_active_wiki_pve_catalog(&ctx_a).is_some());
    assert!(get_active_wiki_pve_catalog(&ctx_b).is_none());

    // ctx_a builds authored 3-room layout (gating out generic 2-room fallback).
    let built = build_wiki_deadspace_rooms(
        &ctx_a,
        30_000_142,
        DungeonTemplateId::new(custom_did),
        "Guristas Scout Outpost",
        [100_000.0, 0.0, 0.0],
        true,
    )
    .expect("authored wiki build");
    assert_eq!(built.entry_id, "guristas_scout_outpost_0");
    assert_eq!(built.source_label, "wiki_pve_v2");
    assert_eq!(built.rooms.len(), 3);
    assert_eq!(built.stats.concrete_rows, 4);
    assert_eq!(built.stats.standin_rows, 1);
    assert_eq!(built.stats.skipped_rows, 1);
    assert_eq!(built.stats.container_objects, 1);

    // Room 1: initial wave + sequential wave triggered OnKill(group 1) + UnlockGate
    assert_eq!(built.rooms[0].room_name, "Outer Pocket");
    assert_eq!(built.rooms[0].npc_groups.len(), 2);
    assert!(built.rooms[0].npc_groups[0].spawn_immediately);
    assert!(!built.rooms[0].npc_groups[1].spawn_immediately);
    assert!(built.rooms[0].objects.iter().any(|o| o.is_container && o.type_id == 23));
    assert!(built.rooms[0].triggers.iter().any(|t| matches!(
        t.kind,
        DungeonTriggerKind::OnKill {
            target_npc_group: Some(1),
            ..
        }
    ) && matches!(t.action, DungeonTriggerAction::SpawnNpcGroup(2))));
    assert!(built.rooms[0].triggers.iter().any(|t| matches!(
        t.action,
        DungeonTriggerAction::UnlockGate
    )));

    // Room 2: OnApproach spawn trigger bound to room anchor
    assert!(built.rooms[1].triggers.iter().any(|t| matches!(
        t.kind,
        DungeonTriggerKind::OnApproach { .. }
    ) && matches!(t.action, DungeonTriggerAction::SpawnNpcGroup(1))));

    // Room 3: CompleteDungeon action on kill
    assert!(built.rooms[2].triggers.iter().any(|t| matches!(
        t.action,
        DungeonTriggerAction::CompleteDungeon
    )));

    // ctx_b (with catalog disabled) falls back cleanly to generic 2-room SDE synthetic layout.
    let fallback_rooms = resolve_rooms_with_content_fallback(
        &ctx_b,
        30_000_142,
        custom_did,
        "Guristas Scout Outpost",
        [100_000.0, 0.0, 0.0],
        true,
    );
    assert_eq!(fallback_rooms.len(), 2);

    // Clear test override restores default slot resolution cleanly without panicking.
    clear_wiki_pve_catalog_for_test(&ctx_a);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn spawn_sde_dungeon_runtime_instance_wires_authored_content_and_unique_instance_ids() {
    let mut sde = (*shared_sde()).clone();
    let concrete_npc_tid = pick_concrete_combat_npc_tid(&sde);
    let custom_did = 9_901_496_u32;
    sde.dungeons.insert(
        custom_did,
        DungeonTemplate {
            dungeon_id: custom_did,
            name: "Guristas Staging Annex".into(),
            archetype_id: Some(8),
            faction_id: Some(500_010),
            difficulty: Some(2),
        },
    );
    let sde = Arc::new(sde);
    let ctx = make_ctx(sde);
    let cat = Arc::new(
        parse_and_validate_wiki_pve_json(&minimal_v2_catalog_json(
            custom_did,
            "guristas_staging_annex_0",
            "Guristas Staging Annex",
            concrete_npc_tid,
        ))
        .expect("valid v2 fixture"),
    );
    install_wiki_pve_catalog_for_test(&ctx, cat);

    let inst1 = spawn_sde_dungeon_runtime_instance(
        &ctx,
        90_000_701,
        30_000_142,
        custom_did,
        "Deadspace",
        [20_000_000.0, 0.0, 0.0],
        true,
    )
    .await
    .expect("spawn authored instance 1");
    let inst2 = spawn_sde_dungeon_runtime_instance(
        &ctx,
        90_000_701,
        30_000_142,
        custom_did,
        "Deadspace",
        [30_000_000.0, 0.0, 0.0],
        true,
    )
    .await
    .expect("spawn authored instance 2");

    assert_ne!(inst1, inst2, "PVE-213: distinct dungeon instances must allocate unique IDs");
    read_dungeon_runtime_store(&ctx, |store| {
        let rt1 = store.instances.get(&inst1).expect("inst1 stored");
        assert_eq!(rt1.rooms.len(), 3, "authored 3-room layout instantiated instead of 2-room fallback");
        assert_eq!(rt1.dungeon_id, custom_did);
    });
}

#[test]
fn loads_committed_importer_fixture_and_builds_multi_room_dungeon() {
    let fixture_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/synthetic_wiki_pve_content.json");
    let cat = Arc::new(
        load_wiki_pve_catalog_from_path(&fixture_path)
            .expect("committed importer fixture must parse and validate cleanly"),
    );
    assert_eq!(cat.entries.len(), 7);
    assert_eq!(cat.stats.authored_dungeon_entries, 5);

    let e1 = cat
        .entries
        .iter()
        .find(|e| e.entry_id == "Synthetic Raid#1")
        .expect("Synthetic Raid#1 present");
    assert_eq!(e1.unit_index, 1);
    assert_eq!(e1.unit_kind, "combat_site");
    assert_eq!(e1.sde_matches.dungeon_ids, vec![4001]);
    assert_eq!(e1.rooms.len(), 2);
    let r1_w = &e1.rooms[0].waves;
    assert_eq!(
        r1_w[0].spawns[0]
            .trigger
            .as_ref()
            .and_then(|t| t.kind.as_deref()),
        Some("this_ship_destroyed")
    );
    assert_eq!(
        r1_w[0].spawns[0]
            .trigger
            .as_ref()
            .map(|t| t.target_wave_indices.as_slice()),
        Some([2, 3].as_slice())
    );
    assert_eq!(r1_w[0].spawns[1].destructible, Some(true));
    assert_eq!(r1_w[0].spawns[1].count.min, None);
    assert_eq!(r1_w[0].spawns[1].count.max, None);
    assert_eq!(r1_w[0].spawns[2].destructible, Some(false));
    assert_eq!(r1_w[0].spawns[2].count.min, Some(1));
    assert_eq!(r1_w[0].spawns[2].count.max, None);
    assert!(r1_w[0].spawns[2].resolution.had_ellipsis);
    assert_eq!(
        r1_w[0].spawns[3]
            .trigger
            .as_ref()
            .and_then(|t| t.unlocks_gate),
        Some(true)
    );
    assert_eq!(
        r1_w[1].distance_m,
        Some(WikiWaveDistance {
            min_m: 50_000.0,
            max_m: 80_000.0,
        })
    );
    assert!(!r1_w[1].distance_unparsed);
    assert_eq!(
        r1_w[2].distance_m,
        Some(WikiWaveDistance {
            min_m: 25_000.0,
            max_m: 25_000.0,
        })
    );
    assert_eq!(
        r1_w[2]
            .wave_trigger
            .as_ref()
            .and_then(|t| t.time_seconds),
        Some(45.0)
    );
    assert_eq!(r1_w[3].distance_m, None);
    assert!(r1_w[3].distance_unparsed);
    assert_eq!(
        r1_w[4]
            .wave_trigger
            .as_ref()
            .and_then(|t| t.kind.as_deref()),
        Some("ship_approached")
    );

    let e3 = cat
        .entries
        .iter()
        .find(|e| e.entry_id == "Synthetic Raid#3")
        .expect("Synthetic Raid#3 present");
    assert_eq!(e3.sde_matches.mission_ids, vec![9004]);
    assert_eq!(e3.rooms[0].waves[0].spawns[1].count.min, Some(0));
    assert_eq!(e3.rooms[0].waves[0].spawns[1].count.max, None);
    assert_eq!(e3.rooms[0].waves[0].spawns[1].destructible, Some(false));
    assert_eq!(
        e3.rooms[0].waves[1].spawns[0].resolution.status,
        "resolved_class_candidates"
    );
    assert_eq!(
        e3.rooms[0].waves[1].spawns[0].resolution.candidate_type_ids,
        vec![10010]
    );

    let e4 = cat
        .entries
        .iter()
        .find(|e| e.entry_id == "Synthetic Raid#4")
        .expect("courier entry present");
    assert!(e4.rooms.is_empty());
    let e5 = cat
        .entries
        .iter()
        .find(|e| e.entry_id == "Synthetic Raid#5")
        .expect("mining entry present");
    assert!(e5.rooms.is_empty());
    let e6 = cat
        .entries
        .iter()
        .find(|e| e.entry_id == "Synthetic Raid#6")
        .expect("part 1 entry present");
    assert_eq!(e6.sde_matches.mission_ids, vec![9001]);

    let sde = shared_sde();
    let ctx = make_ctx(sde);
    install_wiki_pve_catalog_for_test(&ctx, cat);

    // Resolve via by_site_name + system pirate faction (30_000_142 = Guristas) so SDE
    // dungeon table contents never shadow the committed fixture's multi-room entry.
    let custom_did = 990_771_u32;
    let rooms = resolve_rooms_with_content_fallback(
        &ctx,
        30_000_142,
        custom_did,
        "Synthetic Raid",
        [10_000_000.0, 0.0, 0.0],
        true,
    );
    assert_eq!(rooms.len(), 2);

    let r1 = &rooms[0];
    assert!(r1.gated, "Room 1 must be gated when has_entry_gate=true");
    assert!(r1.gate_locked);
    assert!(
        r1.objects
            .iter()
            .any(|o| o.is_container && o.name == "Active Cache"),
        "Container row must materialize as ContainerObject"
    );
    assert!(
        !r1.objects.iter().any(|o| o.name == "Zero Depot"),
        "P2-1: explicit 0-count depot must be skipped rather than spawned"
    );
    assert_eq!(r1.npc_groups.len(), 5, "Waves 1..=5 form 5 combat groups");
    assert!(
        !r1.npc_groups[0].npc_specs.iter().any(|(tid, _)| *tid == 17_831),
        "Acceleration Gate (category 2, type 17831) must never spawn as a combat NPC"
    );
    assert!(
        !r1.objects.iter().any(|o| o.name == "Acceleration Gate"),
        "Acceleration Gate must not materialize as a loot container"
    );
    assert!(r1.npc_groups[0].spawn_immediately);
    let g1_count: usize = r1.npc_groups[0].npc_specs.iter().map(|(_, c)| *c).sum();
    assert!(
        (5..=6).contains(&g1_count),
        "Group 1 spawn counts (2 + ?->1 + 1-?->1 + 1..=2) must sum to 5..=6, got {g1_count}"
    );
    assert!(!r1.npc_groups[1].spawn_immediately);
    assert!(!r1.npc_groups[4].spawn_immediately);
    assert!(
        r1.triggers.iter().any(|t| matches!(
            (&t.kind, &t.action),
            (
                DungeonTriggerKind::OnKill {
                    target_npc_group: Some(1),
                    ..
                },
                DungeonTriggerAction::SpawnNpcGroup(2)
            )
        )),
        "sequential Wave 2 must trigger on Wave 1 kill"
    );
    assert!(
        r1.triggers.iter().any(|t| matches!(
            (&t.kind, &t.action),
            (
                DungeonTriggerKind::OnApproach { radius_m, .. },
                DungeonTriggerAction::SpawnNpcGroup(5)
            ) if (*radius_m - 65_000.0).abs() < 1e-6
        )),
        "ship_approached Wave 5 must use 50-80km midpoint (65km) OnApproach trigger"
    );
    let unlock_triggers: Vec<_> = r1
        .triggers
        .iter()
        .filter(|t| {
            t.all_actions()
                .any(|a| matches!(a, DungeonTriggerAction::UnlockGate))
        })
        .collect();
    assert_eq!(
        unlock_triggers.len(),
        1,
        "Wave 1 tagged spawn trigger r1_w1_s4 must emit exactly one UnlockGate trigger without duplicate OnKill(1)"
    );
    assert!(
        matches!(
            (&unlock_triggers[0].kind, &unlock_triggers[0].action),
            (
                DungeonTriggerKind::OnKillTagged { target_tag, .. },
                DungeonTriggerAction::UnlockGate
            ) if target_tag == "r1_w1_s4"
        ),
        "Wave 1 unlocks_gate spawn trigger must emit OnKillTagged(r1_w1_s4) -> UnlockGate"
    );

    let r2 = &rooms[1];
    assert!(r2.gated);
    assert!(!r2.gate_locked);
    assert!(
        r2.objects
            .iter()
            .any(|o| o.is_container && o.name == "Angel Bunker" && o.type_id == 16_757),
        "Structure row with string loot must materialize with its resolved type_id"
    );
    assert_eq!(
        r2.npc_groups.len(),
        1,
        "Static Structures had only containers/0-? props; Wave 2 forms the sole combat group"
    );
    assert!(
        r2.npc_groups[0].spawn_immediately,
        "BF-02: first combat wave after non-combat Wave 1 must promote to spawn_immediately"
    );
    assert_eq!(
        r2.npc_groups[0].npc_specs.iter().map(|(_, c)| *c).sum::<usize>(),
        2
    );
    assert!(
        r2.triggers
            .iter()
            .any(|t| matches!(t.action, DungeonTriggerAction::CompleteDungeon))
    );
    assert_eq!(
        r1.gate_destination_pos,
        [r1.position[0] - 10_000.0, r1.position[1], r1.position[2]],
        "Room 1 gate_destination_pos must remain Room 1 arrival coordinate"
    );
    assert_eq!(r1.gate_links.len(), 1);
    assert_eq!(
        r1.gate_links[0].destination_pos,
        [r2.position[0] - 10_000.0, r2.position[1], r2.position[2]]
    );
    assert!(
        r1.triggers.iter().any(|t| matches!(
            (&t.kind, &t.action),
            (
                DungeonTriggerKind::OnTimer { delay_secs: 45, .. },
                DungeonTriggerAction::SpawnNpcGroup(3)
            )
        )),
        "Wave 3 45s timed trigger must translate to OnTimer(45) -> SpawnNpcGroup(3)"
    );
    assert!(
        r1.triggers.iter().any(|t| matches!(
            (&t.kind, &t.action),
            (
                DungeonTriggerKind::OnTimer { delay_secs: 15, .. },
                DungeonTriggerAction::SpawnNpcGroup(4)
            )
        )),
        "Wave 4 15s timed trigger must translate to OnTimer(15) -> SpawnNpcGroup(4)"
    );
    assert!(
        r1.triggers.iter().any(|t| matches!(
            (&t.kind, &t.action),
            (
                DungeonTriggerKind::OnKillTagged { target_tag, .. },
                DungeonTriggerAction::StartEscalation { chance_bps: 10_000, .. }
            ) if target_tag == "r1_w2_s1"
        )),
        "Wave 2 escalation spawn must emit OnKillTagged -> StartEscalation"
    );
    assert!(
        r1.npc_groups[0]
            .npc_entries
            .iter()
            .any(|e| e.tag.as_deref() == Some("r1_w1_s1")),
        "Wave 1 npc_entries must preserve authored trigger_ship_tag"
    );

    let rooms_no_entry_gate = resolve_rooms_with_content_fallback(
        &ctx,
        30_000_142,
        custom_did,
        "Synthetic Raid",
        [10_000_000.0, 0.0, 0.0],
        false,
    );
    assert!(!rooms_no_entry_gate[0].gated);
    assert!(rooms_no_entry_gate[1].gated);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn multi_wave_dungeon_without_unlocks_gate_spawns_wave2_before_gate_unlock_and_completion() {
    let sde = shared_sde();
    let ctx = make_ctx(sde.clone());
    let concrete_npc_tid = pick_concrete_combat_npc_tid(&sde);
    let custom_did = 990_772_u32;
    let sys_id = 30_000_142_u32;
    let char_id = 90_000_702_i64;

    let multi_wave_json = format!(
        r#"{{
  "header": {{"schema_version": 2, "source": "EVE University Wiki (CC BY-SA 4.0)", "license": "CC BY-SA 4.0"}},
  "entries": [{{
    "entry_id": "multi_wave_no_unlock_flag",
    "base_title": "Multi-Wave Complex",
    "sde_matches": {{"dungeon_ids": [{custom_did}], "mission_ids": []}},
    "rooms": [
      {{
        "room_index": 1,
        "room_name": "Outer Pocket",
        "has_acceleration_gate": true,
        "waves": [
          {{
            "wave_index": 1,
            "timing": "initial_spawn",
            "spawns": [{{
              "raw_class": "Frigate",
              "raw_name": "Wave 1 Guard",
              "count": {{"min": 1, "max": 1}},
              "resolution": {{"status": "resolved_exact", "type_ids": [{concrete_npc_tid}], "candidate_type_ids": [], "had_ellipsis": false}}
            }}]
          }},
          {{
            "wave_index": 2,
            "timing": "sequential_wave",
            "wave_trigger": {{"kind": "last_in_group_destroyed", "source_wave_index": 1}},
            "spawns": [{{
              "raw_class": "Cruiser",
              "raw_name": "Wave 2 Reinforcement",
              "count": {{"min": 1, "max": 1}},
              "resolution": {{"status": "resolved_exact", "type_ids": [{concrete_npc_tid}], "candidate_type_ids": [], "had_ellipsis": false}}
            }}]
          }}
        ]
      }},
      {{
        "room_index": 2,
        "room_name": "Final Chamber",
        "has_acceleration_gate": false,
        "waves": [
          {{
            "wave_index": 1,
            "timing": "initial_spawn",
            "spawns": [{{
              "raw_class": "Cruiser",
              "raw_name": "Chamber Wave 1",
              "count": {{"min": 1, "max": 1}},
              "resolution": {{"status": "resolved_exact", "type_ids": [{concrete_npc_tid}], "candidate_type_ids": [], "had_ellipsis": false}}
            }}]
          }},
          {{
            "wave_index": 2,
            "timing": "sequential_wave",
            "wave_trigger": {{"kind": "group_destroyed", "source_wave_index": 1}},
            "spawns": [{{
              "raw_class": "Battleship",
              "raw_name": "Chamber Overseer",
              "count": {{"min": 1, "max": 1}},
              "resolution": {{"status": "resolved_exact", "type_ids": [{concrete_npc_tid}], "candidate_type_ids": [], "had_ellipsis": false}}
            }}]
          }}
        ]
      }}
    ]
  }}],
  "indexes": {{"by_dungeon_id": {{"{custom_did}": ["multi_wave_no_unlock_flag"]}}}}
}}"#
    );
    let cat = Arc::new(parse_and_validate_wiki_pve_json(&multi_wave_json).expect("valid multi-wave json"));
    install_wiki_pve_catalog_for_test(&ctx, cat);

    let inst_id = spawn_sde_dungeon_runtime_instance(
        &ctx,
        char_id,
        sys_id,
        custom_did,
        "Deadspace",
        [20_000_000.0, 0.0, 0.0],
        true,
    )
    .await
    .expect("spawn multi-wave dungeon");

    let r0_w1_npc = read_dungeon_runtime_store(&ctx, |store| {
        let rt = store.instances.get(&inst_id).expect("inst stored");
        assert!(rt.rooms[0].gate_locked, "room 0 gate starts locked");
        assert!(rt.rooms[0].npc_groups[0].spawned);
        assert!(!rt.rooms[0].npc_groups[1].spawned);
        rt.rooms[0].npc_groups[0].npc_ball_ids[0]
    });

    on_dungeon_room_entered(&ctx, sys_id, inst_id, 0, char_id).await;
    // Clear Room 0 Wave 1: Wave 2 must spawn, and gate must REMAIN LOCKED until Wave 2 dies.
    on_dungeon_npc_killed(&ctx, sys_id, r0_w1_npc, char_id).await;
    let r0_w2_npc = read_dungeon_runtime_store(&ctx, |store| {
        let rt = store.instances.get(&inst_id).expect("inst stored");
        let r0 = &rt.rooms[0];
        assert!(r0.npc_groups[1].spawned, "BF-02: Wave 2 must spawn on Wave 1 clear");
        assert!(r0.gate_locked, "BF-02: gate must NOT unlock while Room 0 Wave 2 is alive");
        assert!(!r0.cleared);
        assert!(!rt.completed && !rt.despawned);
        r0.npc_groups[1].npc_ball_ids[0]
    });

    // Clear Room 0 Wave 2: gate unlocks now.
    on_dungeon_npc_killed(&ctx, sys_id, r0_w2_npc, char_id).await;
    read_dungeon_runtime_store(&ctx, |store| {
        let rt = store.instances.get(&inst_id).expect("inst stored");
        assert!(!rt.rooms[0].gate_locked, "room 0 gate unlocks after final wave dies");
        assert!(rt.rooms[0].cleared);
        assert!(!rt.completed && !rt.despawned);
    });

    // Advance into Room 1 (final room) and kill Wave 1: Wave 2 must spawn without premature completion/despawn.
    materialize_dungeon_room_entities(&ctx, sys_id, inst_id, 1)
        .await
        .expect("materialize room 1");
    on_dungeon_room_entered(&ctx, sys_id, inst_id, 1, char_id).await;
    let r1_w1_npc = read_dungeon_runtime_store(&ctx, |store| {
        let r1 = &store.instances.get(&inst_id).unwrap().rooms[1];
        assert!(r1.npc_groups[0].spawned);
        assert!(!r1.npc_groups[1].spawned);
        r1.npc_groups[0].npc_ball_ids[0]
    });

    on_dungeon_npc_killed(&ctx, sys_id, r1_w1_npc, char_id).await;
    let r1_w2_npc = read_dungeon_runtime_store(&ctx, |store| {
        let rt = store.instances.get(&inst_id).expect("inst still active");
        assert!(!rt.completed && !rt.despawned, "BF-02: final room Wave 1 clear must not prematurely complete/despawn dungeon");
        let r1 = &rt.rooms[1];
        assert!(r1.npc_groups[1].spawned, "BF-02: final room Wave 2 must spawn");
        r1.npc_groups[1].npc_ball_ids[0]
    });

    on_dungeon_npc_killed(&ctx, sys_id, r1_w2_npc, char_id).await;
    read_dungeon_runtime_store(&ctx, |store| {
        let rt = store.instances.get(&inst_id).expect("inst retained in store");
        assert!(rt.completed && rt.despawned, "dungeon completes & despawns after final room Wave 2 clear");
    });
}

#[test]
fn parses_schema_v2_distance_ranges_unparsed_null_and_skips_zero_count_containers() {
    let sde = shared_sde();
    let ctx = make_ctx(sde.clone());
    let concrete_npc_tid = pick_concrete_combat_npc_tid(&sde);
    let custom_did = 990_775_u32;

    let payload = format!(
        r#"{{
  "header": {{"schema_version": 2, "source": "EVE University Wiki (CC BY-SA 4.0)", "license": "CC BY-SA 4.0"}},
  "entries": [{{
    "entry_id": "schema_v2_dist_and_zero_container",
    "base_title": "Distance Range Site",
    "sde_matches": {{"dungeon_ids": [{custom_did}], "mission_ids": []}},
    "rooms": [{{
      "room_index": 1,
      "room_name": "Pocket One",
      "has_acceleration_gate": false,
      "waves": [
        {{
          "wave_index": 1,
          "wave_name": "Unparsed Null + Container Counts",
          "timing": "initial_spawn",
          "distance_m": null,
          "distance_unparsed": true,
          "spawns": [
            {{
              "raw_class": "Depot",
              "raw_name": "Zero Count Depot",
              "count": {{"min": 0, "max": 0}},
              "resolution": {{"status": "resolved_structure_faction", "type_ids": [23], "candidate_type_ids": [], "had_ellipsis": false}}
            }},
            {{
              "raw_class": "Container",
              "raw_name": "Active Cache",
              "count": {{"min": 1, "max": 1}},
              "resolution": {{"status": "resolved_structure_faction", "type_ids": [23], "candidate_type_ids": [], "had_ellipsis": false}}
            }},
            {{
              "raw_class": "Frigate",
              "raw_name": "Pocket Guard",
              "count": {{"min": 1, "max": 1}},
              "resolution": {{"status": "resolved_exact", "type_ids": [{concrete_npc_tid}], "candidate_type_ids": [], "had_ellipsis": false}}
            }}
          ]
        }},
        {{
          "wave_index": 2,
          "wave_name": "Approach Range Wave",
          "timing": "sequential_wave",
          "distance_m": {{"min_m": 50000, "max_m": 80000}},
          "distance_unparsed": false,
          "wave_trigger": {{"kind": "ship_approached"}},
          "spawns": [
            {{
              "raw_class": "Cruiser",
              "raw_name": "Approach Ambush",
              "count": {{"min": 1, "max": 1}},
              "resolution": {{"status": "resolved_exact", "type_ids": [{concrete_npc_tid}], "candidate_type_ids": [], "had_ellipsis": false}}
            }}
          ]
        }},
        {{
          "wave_index": 3,
          "wave_name": "Legacy Scalar Distance",
          "timing": "static_structures",
          "distance_m": 2600.0,
          "spawns": [
            {{
              "raw_class": "Container",
              "raw_name": "Legacy Scalar Cache",
              "count": {{"min": null, "max": null}},
              "resolution": {{"status": "resolved_structure_faction", "type_ids": [23], "candidate_type_ids": [], "had_ellipsis": false}}
            }}
          ]
        }}
      ]
    }}]
  }}],
  "indexes": {{"by_dungeon_id": {{"{custom_did}": ["schema_v2_dist_and_zero_container"]}}}}
}}"#
    );

    let cat = Arc::new(parse_and_validate_wiki_pve_json(&payload).expect("canonical v2 distance_m parses"));
    let waves = &cat.entries[0].rooms[0].waves;
    assert!(waves[0].distance_m.is_none());
    assert!(waves[0].distance_unparsed);
    let range = waves[1].distance_m.expect("range distance parsed");
    assert_eq!(range, WikiWaveDistance { min_m: 50_000.0, max_m: 80_000.0 });
    assert!((range.effective_m() - 65_000.0).abs() < 1e-6);
    assert_eq!(waves[2].distance_m, Some(WikiWaveDistance { min_m: 2_600.0, max_m: 2_600.0 }));

    install_wiki_pve_catalog_for_test(&ctx, cat);
    let room_pos = [10_000_000.0, 0.0, 0.0];
    let rooms = resolve_rooms_with_content_fallback(
        &ctx,
        30_000_142,
        custom_did,
        "Distance Range Site",
        room_pos,
        false,
    );
    assert_eq!(rooms.len(), 1);
    let r = &rooms[0];

    let containers: Vec<&str> = r
        .objects
        .iter()
        .filter(|o| o.is_container)
        .map(|o| o.name.as_str())
        .collect();
    assert_eq!(
        containers,
        vec!["Active Cache", "Legacy Scalar Cache"],
        "P2-1: explicit zero-count Depot must be skipped while non-zero/null containers materialize"
    );
    let legacy_cache = r
        .objects
        .iter()
        .find(|o| o.name == "Legacy Scalar Cache")
        .expect("legacy scalar container present");
    let dx = legacy_cache.position[0] - room_pos[0];
    let dz = legacy_cache.position[2] - room_pos[2];
    assert!(((dx * dx + dz * dz).sqrt() - 2_600.0).abs() < 1e-6);

    let approach_radius = r
        .triggers
        .iter()
        .find_map(|t| match t.kind {
            DungeonTriggerKind::OnApproach { radius_m, .. } => Some(radius_m),
            _ => None,
        })
        .expect("OnApproach trigger synthesized for Wave 2");
    assert!(
        (approach_radius - 65_000.0).abs() < 1e-6,
        "BF-01: OnApproach radius must use effective_m() midpoint 65_000.0, got {approach_radius}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wiki_dungeon_without_entry_gate_remains_warpable_when_room1_has_outgoing_gate() {
    let sde = shared_sde();
    let ctx = make_ctx(sde.clone());
    let concrete_npc_tid = pick_concrete_combat_npc_tid(&sde);
    let custom_did = 990_776_u32;
    let sys_id = 30_000_142_u32;
    let char_id = 90_000_776_i64;
    let base_pos = [20_000_000.0, 0.0, 0.0];

    let payload = format!(
        r#"{{
  "header": {{"schema_version": 2, "source": "EVE University Wiki (CC BY-SA 4.0)", "license": "CC BY-SA 4.0"}},
  "entries": [{{
    "entry_id": "open_entry_with_room1_outgoing_gate",
    "base_title": "Open Entry Site",
    "sde_matches": {{"dungeon_ids": [{custom_did}], "mission_ids": []}},
    "rooms": [
      {{
        "room_index": 1,
        "room_name": "Ungated Entry Pocket",
        "has_acceleration_gate": true,
        "waves": [{{
          "wave_index": 1,
          "timing": "initial_spawn",
          "spawns": [{{
            "raw_class": "Frigate",
            "raw_name": "Pocket Scout",
            "count": {{"min": 1, "max": 1}},
            "resolution": {{"status": "resolved_exact", "type_ids": [{concrete_npc_tid}], "candidate_type_ids": [], "had_ellipsis": false}}
          }}]
        }}]
      }},
      {{
        "room_index": 2,
        "room_name": "Gated Inner Pocket",
        "has_acceleration_gate": false,
        "waves": [{{
          "wave_index": 1,
          "timing": "initial_spawn",
          "spawns": [{{
            "raw_class": "Cruiser",
            "raw_name": "Inner Guardian",
            "count": {{"min": 1, "max": 1}},
            "resolution": {{"status": "resolved_exact", "type_ids": [{concrete_npc_tid}], "candidate_type_ids": [], "had_ellipsis": false}}
          }}]
        }}]
      }}
    ]
  }}],
  "indexes": {{"by_dungeon_id": {{"{custom_did}": ["open_entry_with_room1_outgoing_gate"]}}}}
}}"#
    );
    let cat = Arc::new(parse_and_validate_wiki_pve_json(&payload).expect("valid open-entry payload"));
    install_wiki_pve_catalog_for_test(&ctx, cat);

    let inst_id = spawn_sde_dungeon_runtime_instance(
        &ctx,
        char_id,
        sys_id,
        custom_did,
        "Deadspace",
        base_pos,
        false,
    )
    .await
    .expect("spawn ungated-entry wiki dungeon");

    let room0_pos = read_dungeon_runtime_store(&ctx, |store| {
        let rt = store.instances.get(&inst_id).expect("instance stored");
        assert!(rt.entry_gate_pos.is_none());
        assert_eq!(rt.rooms.len(), 2);
        assert!(
            !rt.rooms[0].gated,
            "BF-02: Room 1 must stay ungated when has_entry_gate is false even with outgoing gate"
        );
        assert!(rt.rooms[1].gated, "Room 2 must still require Room 1 acceleration gate");
        rt.rooms[0].position
    });

    let warp_target = resolve_deadspace_or_escalation_warp_target(
        &ctx,
        char_id,
        sys_id,
        "dungeon",
        inst_id,
        None,
    )
    .expect("BF-02: warping to Room 1 without entry gate must not fail with NaturalPhenomenaDisruptingWarp");
    let pos = warp_target.expect("warp target resolved for Room 1");
    assert_eq!([pos.x, pos.y, pos.z], room0_pos);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wiki_entry_gate_lands_at_room1_not_room2_and_preserves_gate_links() {
    use eve_machonet::SessionState;
    use eve_proto::PyRep;

    let sde = shared_sde();
    let ctx = make_ctx(Arc::clone(&sde));
    let concrete_npc_tid = pick_concrete_combat_npc_tid(&sde);
    let custom_did = 990_781_u32;
    let sys_id = 30_000_142_u32;
    let char_id = 90_000_781_i64;
    let ship_id = 1_020_781_001_i64;
    let base_pos = [40_000_000.0, 0.0, 0.0];

    ctx.db
        .provision_gm_account("wiki_gate_runner", "pw", char_id, "WikiGateRunner");
    let uid = ctx.db.with_snapshot(|s| s.characters[&char_id].user_id);
    ctx.db
        .acquire_online_lock_with_computer_hash(
            uid,
            char_id as u64,
            Some(char_id),
            None,
            true,
            1_776_400_000,
        )
        .expect("online lock");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship_id;
        }
    });

    let payload = format!(
        r#"{{
  "header": {{"schema_version": 2, "source": "EVE University Wiki (CC BY-SA 4.0)", "license": "CC BY-SA 4.0"}},
  "entries": [{{
    "entry_id": "r3_p1_1_gate_links_site",
    "base_title": "Gate Destination Complex",
    "objective": {{
      "objective_id": 88101,
      "key": "clear_complex",
      "title": "Clear the Gate Destination Complex",
      "kind": "kill_all"
    }},
    "sde_matches": {{"dungeon_ids": [{custom_did}], "mission_ids": []}},
    "rooms": [
      {{
        "room_index": 1,
        "room_name": "Arrival Pocket",
        "has_acceleration_gate": true,
        "gates": [{{
          "target_room_index": 2,
          "label": "Warp to Vault",
          "locked": false
        }}],
        "waves": [{{
          "wave_index": 1,
          "timing": "initial_spawn",
          "spawns": [{{
            "raw_class": "Frigate",
            "raw_name": "Outer Guard",
            "tag": "outer_guard",
            "count": {{"min": 1, "max": 1}},
            "resolution": {{"status": "resolved_exact", "type_ids": [{concrete_npc_tid}], "candidate_type_ids": [], "had_ellipsis": false}}
          }}]
        }}]
      }},
      {{
        "room_index": 2,
        "room_name": "Inner Vault",
        "has_acceleration_gate": false,
        "waves": [{{
          "wave_index": 1,
          "timing": "initial_spawn",
          "spawns": [{{
            "raw_class": "Cruiser",
            "raw_name": "Vault Warden",
            "count": {{"min": 1, "max": 1}},
            "resolution": {{"status": "resolved_exact", "type_ids": [{concrete_npc_tid}], "candidate_type_ids": [], "had_ellipsis": false}}
          }}]
        }}]
      }}
    ]
  }}],
  "indexes": {{"by_dungeon_id": {{"{custom_did}": ["r3_p1_1_gate_links_site"]}}}}
}}"#
    );
    let cat = Arc::new(parse_and_validate_wiki_pve_json(&payload).expect("valid gate_links payload"));
    install_wiki_pve_catalog_for_test(&ctx, cat);

    let inst_id = spawn_sde_dungeon_runtime_instance(
        &ctx,
        char_id,
        sys_id,
        custom_did,
        "Deadspace",
        base_pos,
        true,
    )
    .await
    .expect("spawn gated wiki dungeon");

    let (entry_gate_id, r0_pos, r1_pos, link_gate_id, link_gate_pos) =
        read_dungeon_runtime_store(&ctx, |store| {
            let rt = store.instances.get(&inst_id).expect("instance stored");
            assert_eq!(rt.objectives.len(), 1);
            assert_eq!(rt.objectives[0].objective_id, 88_101);
            assert_eq!(rt.objectives[0].key, "clear_complex");
            assert_eq!(rt.active_objective_id, 88_101);
            assert_eq!(rt.rooms.len(), 2);

            let r0 = &rt.rooms[0];
            let r1 = &rt.rooms[1];
            assert_eq!(
                r0.gate_destination_pos,
                [r0.position[0] - 10_000.0, r0.position[1], r0.position[2]],
                "Room 1 gate_destination_pos must remain Room 1 arrival pos"
            );
            assert_ne!(r0.gate_destination_pos, r1.gate_destination_pos);
            assert_eq!(r0.gate_links.len(), 1);
            assert_eq!(r0.gate_links[0].target_room_index, 1);
            assert_eq!(r0.gate_links[0].label, "Warp to Vault");
            assert!(!r0.gate_links[0].locked);
            assert_eq!(
                r0.gate_links[0].destination_pos,
                [r1.position[0] - 10_000.0, r1.position[1], r1.position[2]]
            );

            (
                rt.entry_gate_ball_id.expect("entry gate materialized"),
                r0.position,
                r1.position,
                r0.gate_links[0]
                    .gate_ball_id
                    .expect("room 1 link gate materialized"),
                r0.gate_links[0].gate_position,
            )
        });

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    sys_handle
        .spawn_ball(eve_destiny::Ball {
            id: ship_id,
            type_id: 587,
            position: glam::DVec3::from_array(base_pos),
            velocity: glam::DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        })
        .await
        .expect("spawn ship ball");

    let mut sess = SessionState::new(921);
    sess.user_id = Some(uid);
    sess.character_id = Some(char_id);
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);

    let enter_r1 = try_activate_dungeon_runtime_gate(
        &ctx,
        &mut sess,
        sys_id,
        &[PyRep::Integer(entry_gate_id)],
    )
    .await
    .expect("entry gate handled")
    .expect("entry gate activation succeeded");
    assert_eq!(
        enter_r1.dict_get("roomIndex").and_then(|v| v.as_i64()),
        Some(1)
    );
    let (_, after_entry_ball) = sys_handle
        .get_ball_snapshot(ship_id)
        .await
        .expect("ball snapshot")
        .expect("ship ball exists");
    assert_eq!(
        after_entry_ball.position.to_array(),
        [r0_pos[0] - 10_000.0, r0_pos[1], r0_pos[2]],
        "activating entry gate must land ship at Room 1 arrival pos, never Room 2"
    );
    read_dungeon_runtime_store(&ctx, |store| {
        assert_eq!(
            store.instances[&inst_id].active_objective_id, 88_101,
            "activating gate must preserve active authored objective_id"
        );
    });

    assert!(
        sys_handle
            .set_ball_position(ship_id, glam::DVec3::from_array(link_gate_pos))
            .await,
        "move ship to room 1 outgoing gate"
    );
    let enter_r2 = try_activate_dungeon_runtime_gate(
        &ctx,
        &mut sess,
        sys_id,
        &[PyRep::Integer(link_gate_id)],
    )
    .await
    .expect("room 1 link gate handled")
    .expect("room 1 link gate activation succeeded");
    assert_eq!(
        enter_r2.dict_get("roomIndex").and_then(|v| v.as_i64()),
        Some(2)
    );
    let (_, after_link_ball) = sys_handle
        .get_ball_snapshot(ship_id)
        .await
        .expect("ball snapshot")
        .expect("ship ball exists");
    assert_eq!(
        after_link_ball.position.to_array(),
        [r1_pos[0] - 10_000.0, r1_pos[1], r1_pos[2]],
        "Room 1 outgoing gate_link must land ship at Room 2 arrival pos"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wiki_escalation_triggers_preserve_chance_bps_fraction_zero_and_default() {
    let sde = shared_sde();
    let ctx = make_ctx(Arc::clone(&sde));
    let concrete_npc_tid = pick_concrete_combat_npc_tid(&sde);
    let sys_id = 30_000_142_u32;

    assert_eq!(resolve_escalation_chance_bps(Some(7_500), None), Ok(7_500));
    assert_eq!(resolve_escalation_chance_bps(None, Some(0.25)), Ok(2_500));
    assert_eq!(resolve_escalation_chance_bps(Some(0), None), Ok(0));
    assert_eq!(resolve_escalation_chance_bps(Some(15_000), None), Ok(10_000));
    assert_eq!(resolve_escalation_chance_bps(None, None), Ok(10_000));
    assert!(resolve_escalation_chance_bps(None, Some(-0.1)).is_err());
    assert!(resolve_escalation_chance_bps(None, Some(1.2)).is_err());
    assert!(resolve_escalation_chance_bps(None, Some(f64::NAN)).is_err());

    let did_bps = 990_782_u32;
    let did_frac = 990_783_u32;
    let did_default = 990_784_u32;
    let did_zero = 990_785_u32;
    let payload = format!(
        r#"{{
  "header": {{"schema_version": 2, "source": "EVE University Wiki (CC BY-SA 4.0)", "license": "CC BY-SA 4.0"}},
  "entries": [
    {{
      "entry_id": "esc_bps_entry",
      "base_title": "Escalation BPS Site",
      "sde_matches": {{"dungeon_ids": [{did_bps}], "mission_ids": []}},
      "rooms": [{{
        "room_index": 1,
        "waves": [{{
          "wave_index": 1,
          "timing": "initial_spawn",
          "spawns": [{{
            "raw_class": "Battleship",
            "raw_name": "Boss BPS",
            "tag": "boss_bps",
            "count": {{"min": 1, "max": 1}},
            "resolution": {{"status": "resolved_exact", "type_ids": [{concrete_npc_tid}], "candidate_type_ids": [], "had_ellipsis": false}},
            "trigger": {{
              "kind": "this_ship_destroyed",
              "trigger_ship_tag": "boss_bps",
              "action": {{
                "kind": "start_escalation",
                "target_dungeon_id": 7001,
                "chance_bps": 7500,
                "target_title": "Escalation Stage 2"
              }}
            }}
          }}]
        }}]
      }}]
    }},
    {{
      "entry_id": "esc_frac_entry",
      "base_title": "Escalation Fraction Site",
      "sde_matches": {{"dungeon_ids": [{did_frac}], "mission_ids": []}},
      "rooms": [{{
        "room_index": 1,
        "waves": [{{
          "wave_index": 1,
          "timing": "initial_spawn",
          "spawns": [{{
            "raw_class": "Battleship",
            "raw_name": "Boss Frac",
            "tag": "boss_frac",
            "count": {{"min": 1, "max": 1}},
            "resolution": {{"status": "resolved_exact", "type_ids": [{concrete_npc_tid}], "candidate_type_ids": [], "had_ellipsis": false}},
            "trigger": {{
              "kind": "this_ship_destroyed",
              "trigger_ship_tag": "boss_frac",
              "is_escalation": true,
              "escalation_chance": 0.25,
              "escalation_dungeon_id": 7002,
              "escalation_title": "Quarter Odds Escalation"
            }}
          }}]
        }}]
      }}]
    }},
    {{
      "entry_id": "esc_default_entry",
      "base_title": "Escalation Default Site",
      "sde_matches": {{"dungeon_ids": [{did_default}], "mission_ids": []}},
      "rooms": [{{
        "room_index": 1,
        "waves": [{{
          "wave_index": 1,
          "timing": "initial_spawn",
          "spawns": [{{
            "raw_class": "Battleship",
            "raw_name": "Boss Default",
            "tag": "boss_default",
            "count": {{"min": 1, "max": 1}},
            "resolution": {{"status": "resolved_exact", "type_ids": [{concrete_npc_tid}], "candidate_type_ids": [], "had_ellipsis": false}},
            "trigger": {{
              "kind": "this_ship_destroyed",
              "trigger_ship_tag": "boss_default",
              "is_escalation": true,
              "escalation_dungeon_id": 7003
            }}
          }}]
        }}]
      }}]
    }},
    {{
      "entry_id": "esc_zero_entry",
      "base_title": "Escalation Zero Odds Site",
      "sde_matches": {{"dungeon_ids": [{did_zero}], "mission_ids": []}},
      "rooms": [{{
        "room_index": 1,
        "waves": [{{
          "wave_index": 1,
          "timing": "initial_spawn",
          "spawns": [{{
            "raw_class": "Battleship",
            "raw_name": "Boss Zero",
            "tag": "boss_zero",
            "count": {{"min": 1, "max": 1}},
            "resolution": {{"status": "resolved_exact", "type_ids": [{concrete_npc_tid}], "candidate_type_ids": [], "had_ellipsis": false}},
            "trigger": {{
              "kind": "this_ship_destroyed",
              "trigger_ship_tag": "boss_zero",
              "action": {{
                "kind": "start_escalation",
                "target_dungeon_id": 7004,
                "chance_bps": 0
              }}
            }}
          }}]
        }}]
      }}]
    }}
  ]
}}"#
    );
    let cat = Arc::new(parse_and_validate_wiki_pve_json(&payload).expect("valid escalation JSON"));
    install_wiki_pve_catalog_for_test(&ctx, cat);

    let b_bps = build_wiki_deadspace_rooms(
        &ctx,
        sys_id,
        DungeonTemplateId::new(did_bps),
        "Escalation BPS Site",
        [10_000_000.0, 0.0, 0.0],
        false,
    )
    .expect("bps build");
    assert!(b_bps.rooms[0].triggers.iter().any(|t| {
        t.action
            == DungeonTriggerAction::StartEscalation {
                target_dungeon_id: Some(7001),
                chance_bps: 7_500,
                target_title: Some("Escalation Stage 2".to_string()),
            }
    }));

    let b_frac = build_wiki_deadspace_rooms(
        &ctx,
        sys_id,
        DungeonTemplateId::new(did_frac),
        "Escalation Fraction Site",
        [10_000_000.0, 0.0, 0.0],
        false,
    )
    .expect("frac build");
    assert!(b_frac.rooms[0].triggers.iter().any(|t| {
        t.action
            == DungeonTriggerAction::StartEscalation {
                target_dungeon_id: Some(7002),
                chance_bps: 2_500,
                target_title: Some("Quarter Odds Escalation".to_string()),
            }
    }));

    let b_default = build_wiki_deadspace_rooms(
        &ctx,
        sys_id,
        DungeonTemplateId::new(did_default),
        "Escalation Default Site",
        [10_000_000.0, 0.0, 0.0],
        false,
    )
    .expect("default build");
    assert!(b_default.rooms[0].triggers.iter().any(|t| {
        t.action
            == DungeonTriggerAction::StartEscalation {
                target_dungeon_id: Some(7003),
                chance_bps: 10_000,
                target_title: None,
            }
    }));

    // Verify chance_bps=0 never rolls an escalation at runtime.
    let inst_id = spawn_sde_dungeon_runtime_instance(
        &ctx,
        90_000_785,
        sys_id,
        did_zero,
        "Deadspace",
        [50_000_000.0, 0.0, 0.0],
        false,
    )
    .await
    .expect("spawn zero-odds dungeon");
    let boss_ball_id = read_dungeon_runtime_store(&ctx, |store| {
        let rt = &store.instances[&inst_id];
        assert!(rt.rooms[0].triggers.iter().any(|t| matches!(
            t.action,
            DungeonTriggerAction::StartEscalation { chance_bps: 0, .. }
        )));
        rt.rooms[0].npc_groups[0].npc_ball_ids[0]
    });
    on_dungeon_npc_killed(&ctx, sys_id, boss_ball_id, 90_000_785).await;
    read_dungeon_runtime_store(&ctx, |store| {
        let rt = &store.instances[&inst_id];
        assert!(rt.escalation_instance_id.is_none());
    });

    let bad_neg = payload.replace("\"escalation_chance\": 0.25", "\"escalation_chance\": -0.25");
    assert!(parse_and_validate_wiki_pve_json(&bad_neg)
        .unwrap_err()
        .contains("escalation_chance"));
}

#[test]
fn rejects_unimplemented_and_unknown_trigger_kinds_with_counted_reasons() {
    let sde = shared_sde();
    let ctx = make_ctx(Arc::clone(&sde));
    let concrete_npc_tid = pick_concrete_combat_npc_tid(&sde);

    let payload = format!(
        r#"{{
  "header": {{"schema_version": 2, "source": "EVE University Wiki (CC BY-SA 4.0)", "license": "CC BY-SA 4.0"}},
  "indexes": {{
    "by_dungeon_id": {{
      "991001": ["valid_site_1"],
      "991002": ["valid_site_2"],
      "991003": ["rej_structure"],
      "991004": ["rej_can"],
      "991005": ["rej_cargo"],
      "991006": ["rej_ore_compound"],
      "991007": ["rej_unknown"]
    }}
  }},
  "entries": [
    {{
      "entry_id": "valid_site_1",
      "base_title": "Valid Complex One",
      "sde_matches": {{"dungeon_ids": [991001], "mission_ids": []}},
      "rooms": [{{
        "room_index": 1,
        "waves": [
          {{
            "wave_index": 1,
            "timing": "initial_spawn",
            "spawns": [{{
              "raw_class": "Frigate",
              "count": {{"min": 1, "max": 1}},
              "resolution": {{"status": "resolved_exact", "type_ids": [{concrete_npc_tid}], "candidate_type_ids": [], "had_ellipsis": false}}
            }}]
          }},
          {{
            "wave_index": 2,
            "timing": "sequential_wave",
            "wave_trigger": {{ "kind": "timed", "time_seconds": 30 }},
            "spawns": [{{
              "raw_class": "Cruiser",
              "count": {{"min": 1, "max": 1}},
              "resolution": {{"status": "resolved_exact", "type_ids": [{concrete_npc_tid}], "candidate_type_ids": [], "had_ellipsis": false}}
            }}]
          }}
        ]
      }}]
    }},
    {{
      "entry_id": "rej_structure",
      "base_title": "Rejected Structure Trigger",
      "sde_matches": {{"dungeon_ids": [991003], "mission_ids": []}},
      "rooms": [{{
        "room_index": 1,
        "waves": [{{
          "wave_index": 1,
          "timing": "sequential_wave",
          "wave_trigger": {{ "kind": "structure_destroyed" }},
          "spawns": []
        }}]
      }}]
    }},
    {{
      "entry_id": "rej_can",
      "base_title": "Rejected Can Trigger",
      "sde_matches": {{"dungeon_ids": [991004], "mission_ids": []}},
      "rooms": [{{
        "room_index": 1,
        "waves": [{{
          "wave_index": 1,
          "timing": "sequential_wave",
          "wave_trigger": {{ "kind": "can_hacked" }},
          "spawns": []
        }}]
      }}]
    }},
    {{
      "entry_id": "rej_cargo",
      "base_title": "Rejected Cargo Trigger",
      "sde_matches": {{"dungeon_ids": [991005], "mission_ids": []}},
      "rooms": [{{
        "room_index": 1,
        "waves": [{{
          "wave_index": 1,
          "timing": "initial_spawn",
          "spawns": [{{
            "raw_class": "Frigate",
            "trigger": {{ "kind": "cargo_delivered" }}
          }}]
        }}]
      }}]
    }},
    {{
      "entry_id": "rej_ore_compound",
      "base_title": "Rejected Ore Compound Trigger",
      "sde_matches": {{"dungeon_ids": [991006], "mission_ids": []}},
      "rooms": [{{
        "room_index": 1,
        "waves": [{{
          "wave_index": 1,
          "timing": "sequential_wave",
          "wave_trigger": {{
            "kind": "compound_and",
            "clauses": [
              {{ "kind": "room_entered", "target_room_index": 1 }},
              {{ "kind": "ore_depleted" }}
            ]
          }},
          "spawns": []
        }}]
      }}]
    }},
    {{
      "entry_id": "rej_unknown",
      "base_title": "Rejected Unknown Future Trigger",
      "sde_matches": {{"dungeon_ids": [991007], "mission_ids": []}},
      "rooms": [{{
        "room_index": 1,
        "waves": [{{
          "wave_index": 1,
          "timing": "sequential_wave",
          "wave_trigger": {{ "kind": "unknown_future_trigger" }},
          "spawns": []
        }}]
      }}]
    }},
    {{
      "entry_id": "valid_site_2",
      "base_title": "Valid Complex Two",
      "sde_matches": {{"dungeon_ids": [991002], "mission_ids": []}},
      "rooms": [{{
        "room_index": 1,
        "waves": [{{
          "wave_index": 1,
          "timing": "sequential_wave",
          "wave_trigger": {{
            "kind": "compound_or",
            "clauses": [
              {{ "kind": "npc_damaged", "hull_below_pct": 40 }},
              {{ "kind": "kill_count", "required_kills": 2 }},
              {{ "kind": "loot_taken", "item_type_id": 34 }}
            ]
          }},
          "spawns": [{{
            "raw_class": "Frigate",
            "count": {{"min": 1, "max": 1}},
            "resolution": {{"status": "resolved_exact", "type_ids": [{concrete_npc_tid}], "candidate_type_ids": [], "had_ellipsis": false}}
          }}]
        }}]
      }}]
    }}
  ]
}}"#
    );

    let cat = parse_and_validate_wiki_pve_json(&payload).expect("catalog with unsupported entries loads valid subset");
    assert_eq!(cat.entries.len(), 2);
    assert_eq!(cat.stats.entries_total, 2);
    assert_eq!(cat.stats.rejected_unsupported_trigger_entries, 5);
    assert_eq!(cat.stats.rejected_by_trigger_kind.len(), 5);
    assert_eq!(cat.stats.rejected_by_trigger_kind.get("structure_destroyed"), Some(&1));
    assert_eq!(cat.stats.rejected_by_trigger_kind.get("can_hacked"), Some(&1));
    assert_eq!(cat.stats.rejected_by_trigger_kind.get("cargo_delivered"), Some(&1));
    assert_eq!(cat.stats.rejected_by_trigger_kind.get("ore_depleted"), Some(&1));
    assert_eq!(cat.stats.rejected_by_trigger_kind.get("unknown_future_trigger"), Some(&1));

    assert!(cat.by_entry_id.contains_key("valid_site_1"));
    assert!(cat.by_entry_id.contains_key("valid_site_2"));
    assert!(cat.by_dungeon_id.contains_key(&991_001));
    assert!(cat.by_dungeon_id.contains_key(&991_002));
    assert!(cat.by_site_name.contains_key("valid complex one"));
    assert!(cat.by_site_name.contains_key("valid complex two"));

    for bad_id in ["rej_structure", "rej_can", "rej_cargo", "rej_ore_compound", "rej_unknown"] {
        assert!(!cat.by_entry_id.contains_key(bad_id));
    }
    for bad_did in [991_003_u32, 991_004, 991_005, 991_006, 991_007] {
        assert!(!cat.by_dungeon_id.contains_key(&bad_did));
    }
    for bad_name in [
        "rejected structure trigger",
        "rejected can trigger",
        "rejected cargo trigger",
        "rejected ore compound trigger",
        "rejected unknown future trigger",
    ] {
        assert!(!cat.by_site_name.contains_key(bad_name));
    }

    let empty_w = std::collections::HashMap::new();
    let empty_r = std::collections::HashMap::new();
    for bad_kind in [
        "structure_destroyed",
        "can_hacked",
        "cargo_delivered",
        "ore_depleted",
        "unknown_future_trigger",
    ] {
        let tr = WikiTrigger {
            kind: Some(bad_kind.to_string()),
            ..Default::default()
        };
        let err = translate_wiki_trigger_kind(&tr, None, [0.0; 3], &[], &empty_w, &empty_r, None, false)
            .unwrap_err();
        assert!(err.contains(bad_kind), "expected '{bad_kind}' in error '{err}'");
    }

    // Programmatic catalog bypassing parser still fails closed inside build_wiki_deadspace_rooms.
    let prog_did = 991_008_u32;
    let prog_entry = WikiPveEntry {
        entry_id: "programmatic_bad".to_string(),
        base_title: "Programmatic Bad".to_string(),
        sde_matches: WikiSdeMatches {
            dungeon_ids: vec![prog_did],
            mission_ids: Vec::new(),
        },
        rooms: vec![WikiRoom {
            room_index: 1,
            room_name: "Pocket".to_string(),
            has_acceleration_gate: false,
            gates: Vec::new(),
            waves: vec![WikiWave {
                wave_index: 1,
                timing: "initial_spawn".to_string(),
                spawn_chance_bps: Some(0),
                wave_trigger: Some(WikiTrigger {
                    kind: Some("structure_destroyed".to_string()),
                    ..Default::default()
                }),
                spawns: vec![WikiSpawn {
                    raw_class: "Frigate".to_string(),
                    resolution: WikiSpawnResolution {
                        type_ids: vec![concrete_npc_tid],
                        ..Default::default()
                    },
                    ..Default::default()
                }],
                ..Default::default()
            }],
        }],
        ..Default::default()
    };
    let mut prog_by_id = std::collections::HashMap::new();
    prog_by_id.insert("programmatic_bad".to_string(), 0);
    let mut prog_by_did = std::collections::BTreeMap::new();
    prog_by_did.insert(prog_did, vec![0]);
    let prog_cat = Arc::new(WikiPveCatalog {
        source: "test".to_string(),
        license: "CC BY-SA 4.0".to_string(),
        entries: vec![prog_entry],
        by_entry_id: prog_by_id,
        by_dungeon_id: prog_by_did,
        by_site_name: std::collections::HashMap::new(),
        stats: WikiPveLoadStats::default(),
    });
    install_wiki_pve_catalog_for_test(&ctx, prog_cat);
    assert!(
        build_wiki_deadspace_rooms(
            &ctx,
            30_000_142,
            DungeonTemplateId::new(prog_did),
            "Programmatic Bad",
            [10_000_000.0, 0.0, 0.0],
            false,
        )
        .is_none(),
        "build_wiki_deadspace_rooms must fail closed on programmatic unsupported trigger"
    );
}

#[test]
fn reserved_timer_ids_compound_depth_actions_tags_wave_chance_and_objectives() {
    let sde = shared_sde();
    let ctx = make_ctx(Arc::clone(&sde));
    let concrete_npc_tid = pick_concrete_combat_npc_tid(&sde);
    let empty_w = std::collections::HashMap::new();
    let empty_r = std::collections::HashMap::new();

    // Reserved __ timer_id rejected in both OnTimer trigger and StartTimer action.
    let bad_timer_json = r#"{
        "header": {"schema_version": 2, "source": "test", "license": "CC BY-SA 4.0"},
        "entries": [{
            "entry_id": "bad_timer",
            "rooms": [{
                "room_index": 1,
                "waves": [{
                    "wave_index": 1,
                    "timing": "sequential_wave",
                    "wave_trigger": { "kind": "timed", "time_seconds": 10, "timer_id": "__reserved" },
                    "spawns": []
                }]
            }]
        }]
    }"#;
    assert!(parse_and_validate_wiki_pve_json(bad_timer_json)
        .unwrap_err()
        .contains("__reserved"));

    let bad_empty_timer_json = bad_timer_json.replace("\"__reserved\"", "\"   \"");
    assert!(parse_and_validate_wiki_pve_json(&bad_empty_timer_json)
        .unwrap_err()
        .contains("non-empty"));

    let bad_start_timer_action = r#"{
        "header": {"schema_version": 2, "source": "test", "license": "CC BY-SA 4.0"},
        "entries": [{
            "entry_id": "bad_action_timer",
            "rooms": [{
                "room_index": 1,
                "waves": [{
                    "wave_index": 1,
                    "timing": "initial_spawn",
                    "wave_trigger": {
                        "kind": "this_ship_destroyed",
                        "action": { "kind": "start_timer", "timer_id": "__internal", "delay_secs": 5 }
                    },
                    "spawns": []
                }]
            }]
        }]
    }"#;
    assert!(parse_and_validate_wiki_pve_json(bad_start_timer_action)
        .unwrap_err()
        .contains("__internal"));

    // Empty compound clauses and nesting depth > MAX_TRIGGER_COMPOUND_DEPTH (4) rejected.
    let empty_clauses = WikiTrigger {
        kind: Some("compound_and".to_string()),
        clauses: Vec::new(),
        ..Default::default()
    };
    assert!(translate_wiki_trigger_kind(&empty_clauses, None, [0.0; 3], &[], &empty_w, &empty_r, None, false).is_err());

    let wrap_compound = |inner: WikiTrigger| WikiTrigger {
        kind: Some("compound_and".to_string()),
        clauses: vec![inner],
        ..Default::default()
    };
    let leaf = WikiTrigger {
        kind: Some("room_entered".to_string()),
        ..Default::default()
    };
    let mut depth4 = leaf.clone();
    for _ in 0..MAX_TRIGGER_COMPOUND_DEPTH {
        depth4 = wrap_compound(depth4);
    }
    assert!(translate_wiki_trigger_kind(&depth4, None, [0.0; 3], &[], &empty_w, &empty_r, None, false).is_ok());

    let depth5 = wrap_compound(depth4);
    assert!(
        translate_wiki_trigger_kind(&depth5, None, [0.0; 3], &[], &empty_w, &empty_r, None, false)
            .unwrap_err()
            .contains("MAX_TRIGGER_COMPOUND_DEPTH")
    );

    // Unknown action kind rejected cleanly.
    let bad_action = WikiTriggerActionSpec {
        kind: "nonexistent_action".to_string(),
        ..Default::default()
    };
    assert!(translate_wiki_action_spec(&bad_action, None, None).is_err());

    // All supported action variants translate accurately.
    let mut waves_map = std::collections::HashMap::new();
    waves_map.insert(2usize, 2u8);
    let specs_and_expected = vec![
        (WikiTriggerActionSpec { kind: "unlock_gate".into(), ..Default::default() }, DungeonTriggerAction::UnlockGate),
        (WikiTriggerActionSpec { kind: "lock_gate".into(), ..Default::default() }, DungeonTriggerAction::LockGate),
        (WikiTriggerActionSpec { kind: "unlock_entry_gate".into(), ..Default::default() }, DungeonTriggerAction::UnlockEntryGate),
        (WikiTriggerActionSpec { kind: "spawn_npc_group".into(), target_wave_index: Some(2), ..Default::default() }, DungeonTriggerAction::SpawnNpcGroup(2)),
        (WikiTriggerActionSpec { kind: "despawn_npc_group".into(), target_group_id: Some(1), ..Default::default() }, DungeonTriggerAction::DespawnNpcGroup(1)),
        (WikiTriggerActionSpec { kind: "spawn_object".into(), object_index: Some(0), ..Default::default() }, DungeonTriggerAction::SpawnObject { object_index: 0 }),
        (WikiTriggerActionSpec { kind: "grant_loot".into(), items: vec![WikiLootItemSpec::Tuple(34, 25)], target_object_index: Some(0), ..Default::default() }, DungeonTriggerAction::GrantLoot { items: vec![(34, 25)], target_object_index: Some(0) }),
        (WikiTriggerActionSpec { kind: "grant_reward".into(), isk: Some(500_000), lp: Some(1_200), corp_id: Some(1000125), faction_id: Some(500010), standing_milli: Some(250), ..Default::default() }, DungeonTriggerAction::GrantReward { isk: 500_000, lp: 1_200, corp_id: 1_000_125, faction_id: 500_010, standing_milli: 250 }),
        (WikiTriggerActionSpec { kind: "complete_dungeon".into(), ..Default::default() }, DungeonTriggerAction::CompleteDungeon),
        (WikiTriggerActionSpec { kind: "fail_dungeon".into(), reason: Some("TimedOut".into()), ..Default::default() }, DungeonTriggerAction::FailDungeon { reason: "TimedOut".into() }),
        (WikiTriggerActionSpec { kind: "start_timer".into(), timer_id: Some("reinforce".into()), delay_secs: Some(20), ..Default::default() }, DungeonTriggerAction::StartTimer { timer_id: "reinforce".into(), delay_secs: 20 }),
        (WikiTriggerActionSpec { kind: "change_objective".into(), objective_id: Some(42), objective_key: Some("obj_42".into()), title: Some("Hold Grid".into()), completed: Some(true), ..Default::default() }, DungeonTriggerAction::SetObjective { objective_id: 42, objective_key: "obj_42".into(), title: "Hold Grid".into(), completed: true }),
        (WikiTriggerActionSpec { kind: "start_escalation".into(), target_dungeon_id: Some(888), chance_bps: Some(3333), target_title: Some("Followup".into()), ..Default::default() }, DungeonTriggerAction::StartEscalation { target_dungeon_id: Some(888), chance_bps: 3333, target_title: Some("Followup".into()) }),
        (WikiTriggerActionSpec { kind: "trigger_message".into(), message_type: Some(2), message_id: Some(9001), ..Default::default() }, DungeonTriggerAction::TriggerMessage { message_type: 2, message_id: 9001 }),
    ];
    for (spec, expected) in specs_and_expected {
        assert_eq!(translate_wiki_action_spec(&spec, Some(&waves_map), None).unwrap(), expected);
    }

    // Verify spawn tags, extra_actions, wave spawn_chance_bps=0 skipping, and objectives wiring.
    let rich_did = 991_090_u32;
    let rich_json = format!(
        r#"{{
  "header": {{"schema_version": 2, "source": "EVE University Wiki (CC BY-SA 4.0)", "license": "CC BY-SA 4.0"}},
  "entries": [{{
    "entry_id": "rich_wiki_entry",
    "base_title": "Rich Complex",
    "objectives": [
      {{ "objective_id": 9101, "key": "kill_warden", "title": "Destroy Warden", "kind": "kill_tagged", "room_index": 1, "target_type_id": {concrete_npc_tid}, "target_count": 1 }}
    ],
    "sde_matches": {{"dungeon_ids": [{rich_did}], "mission_ids": []}},
    "rooms": [{{
      "room_index": 1,
      "room_name": "Rich Pocket",
      "waves": [
        {{
          "wave_index": 1,
          "timing": "initial_spawn",
          "spawn_chance_bps": 10000,
          "spawns": [
            {{
              "raw_class": "Container",
              "raw_name": "Hidden Cache",
              "count": {{"min": 1, "max": 1}},
              "resolution": {{"status": "resolved", "type_ids": [23], "candidate_type_ids": [], "had_ellipsis": false}}
            }},
            {{
              "raw_class": "Cruiser",
              "raw_name": "Complex Warden",
              "tag": "warden_tag",
              "count": {{"min": 1, "max": 1}},
              "resolution": {{"status": "resolved_exact", "type_ids": [{concrete_npc_tid}], "candidate_type_ids": [], "had_ellipsis": false}},
              "trigger": {{
                "kind": "this_ship_destroyed",
                "trigger_ship_tag": "warden_tag",
                "action": {{ "kind": "spawn_object", "object_index": 0 }},
                "extra_actions": [
                  {{ "kind": "grant_reward", "isk": 250000, "lp": 500 }},
                  {{ "kind": "change_objective", "objective_id": 9101, "objective_key": "kill_warden", "title": "Destroy Warden", "completed": true }}
                ]
              }}
            }}
          ]
        }},
        {{
          "wave_index": 2,
          "timing": "optional_spawn",
          "spawn_chance_bps": 0,
          "spawns": [{{
            "raw_class": "Frigate",
            "raw_name": "Never Spawns",
            "count": {{"min": 1, "max": 1}},
            "resolution": {{"status": "resolved_exact", "type_ids": [{concrete_npc_tid}], "candidate_type_ids": [], "had_ellipsis": false}}
          }}]
        }}
      ]
    }}]
  }}]
}}"#
    );
    let cat = Arc::new(parse_and_validate_wiki_pve_json(&rich_json).expect("rich entry parses"));
    assert_eq!(effective_wave_spawn_chance_bps(&cat.entries[0].rooms[0].waves[1]), Ok(0));
    let built_objs = build_wiki_objectives(&cat.entries[0], DungeonTemplateId::new(rich_did));
    assert_eq!(built_objs.len(), 1);
    assert_eq!(built_objs[0].objective_id, 9101);
    assert_eq!(built_objs[0].room_index, Some(1));

    install_wiki_pve_catalog_for_test(&ctx, cat);
    let (rooms, objectives) = resolve_rooms_and_objectives_with_content_fallback(
        &ctx,
        30_000_142,
        rich_did,
        "Rich Complex",
        [10_000_000.0, 0.0, 0.0],
        false,
    );
    assert_eq!(objectives.len(), 1);
    assert_eq!(rooms.len(), 1);
    assert_eq!(rooms[0].npc_groups.len(), 1, "wave 2 with spawn_chance_bps=0 must not spawn");
    assert_eq!(rooms[0].npc_groups[0].npc_entries.len(), 1);
    assert_eq!(rooms[0].npc_groups[0].npc_entries[0].tag.as_deref(), Some("warden_tag"));
    assert_eq!(rooms[0].npc_groups[0].npc_entries[0].custom_name.as_deref(), Some("Complex Warden"));
    assert!(rooms[0].objects[0].spawn_on_trigger, "SpawnObject action must mark target object spawn_on_trigger");
    assert!(rooms[0].triggers.iter().any(|t| {
        matches!(&t.kind, DungeonTriggerKind::OnKillTagged { target_tag, .. } if target_tag == "warden_tag")
            && t.action == DungeonTriggerAction::SpawnObject { object_index: 0 }
            && t.extra_actions.len() == 2
    }));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sequential_and_optional_waves_with_none_trigger_synthesize_fallback_spawn_and_clear() {
    let sde = shared_sde();
    let ctx = make_ctx(Arc::clone(&sde));
    let concrete_npc_tid = pick_concrete_combat_npc_tid(&sde);
    let custom_did = 991_101_u32;
    let sys_id = 30_000_142_u32;
    let char_id = 90_001_101_i64;

    let payload = format!(
        r#"{{
  "header": {{"schema_version": 2, "source": "EVE University Wiki (CC BY-SA 4.0)", "license": "CC BY-SA 4.0"}},
  "entries": [{{
    "entry_id": "seq_optional_null_trigger",
    "base_title": "Null Trigger Multiwave",
    "sde_matches": {{"dungeon_ids": [{custom_did}], "mission_ids": []}},
    "rooms": [
      {{
        "room_index": 1,
        "room_name": "Pocket One",
        "has_acceleration_gate": true,
        "waves": [
          {{
            "wave_index": 1,
            "timing": "initial_spawn",
            "spawns": [{{
              "raw_class": "Frigate",
              "count": {{"min": 1, "max": 1}},
              "resolution": {{"status": "resolved_exact", "type_ids": [{concrete_npc_tid}], "candidate_type_ids": [], "had_ellipsis": false}}
            }}]
          }},
          {{
            "wave_index": 2,
            "timing": "sequential_wave",
            "wave_trigger": null,
            "spawns": [{{
              "raw_class": "Cruiser",
              "count": {{"min": 1, "max": 1}},
              "resolution": {{"status": "resolved_exact", "type_ids": [{concrete_npc_tid}], "candidate_type_ids": [], "had_ellipsis": false}}
            }}]
          }},
          {{
            "wave_index": 3,
            "timing": "optional_spawn",
            "wave_trigger": null,
            "spawns": [{{
              "raw_class": "Battleship",
              "count": {{"min": 1, "max": 1}},
              "resolution": {{"status": "resolved_exact", "type_ids": [{concrete_npc_tid}], "candidate_type_ids": [], "had_ellipsis": false}}
            }}]
          }}
        ]
      }},
      {{
        "room_index": 2,
        "room_name": "Inner Chamber",
        "has_acceleration_gate": false,
        "waves": [{{
          "wave_index": 1,
          "timing": "initial_spawn",
          "spawns": [{{
            "raw_class": "Battleship",
            "count": {{"min": 1, "max": 1}},
            "resolution": {{"status": "resolved_exact", "type_ids": [{concrete_npc_tid}], "candidate_type_ids": [], "had_ellipsis": false}}
          }}]
        }}]
      }}
    ]
  }}]
}}"#
    );
    let cat = Arc::new(parse_and_validate_wiki_pve_json(&payload).expect("valid null-trigger multiwave"));
    install_wiki_pve_catalog_for_test(&ctx, cat);

    let inst_id = spawn_sde_dungeon_runtime_instance(
        &ctx,
        char_id,
        sys_id,
        custom_did,
        "Deadspace",
        [25_000_000.0, 0.0, 0.0],
        false,
    )
    .await
    .expect("spawn null-trigger multiwave dungeon");

    let w1_npc = read_dungeon_runtime_store(&ctx, |store| {
        let rt = &store.instances[&inst_id];
        let r0 = &rt.rooms[0];
        assert_eq!(r0.npc_groups.len(), 3);
        assert!(r0.npc_groups[0].spawn_immediately && r0.npc_groups[0].spawned);
        assert!(!r0.npc_groups[1].spawn_immediately && !r0.npc_groups[1].spawned);
        assert!(!r0.npc_groups[2].spawn_immediately && !r0.npc_groups[2].spawned);
        assert!(r0.triggers.iter().any(|t| matches!(
            (&t.kind, &t.action),
            (DungeonTriggerKind::OnKill { target_npc_group: Some(1), .. }, DungeonTriggerAction::SpawnNpcGroup(2))
        )));
        assert!(r0.triggers.iter().any(|t| matches!(
            (&t.kind, &t.action),
            (DungeonTriggerKind::OnKill { target_npc_group: Some(2), .. }, DungeonTriggerAction::SpawnNpcGroup(3))
        )));
        r0.npc_groups[0].npc_ball_ids[0]
    });

    on_dungeon_npc_killed(&ctx, sys_id, w1_npc, char_id).await;
    let w2_npc = read_dungeon_runtime_store(&ctx, |store| {
        let rt = &store.instances[&inst_id];
        assert!(!rt.completed);
        assert!(rt.rooms[0].npc_groups[1].spawned, "Wave 2 spawns after Wave 1 kill");
        assert!(!rt.rooms[0].npc_groups[2].spawned);
        rt.rooms[0].npc_groups[1].npc_ball_ids[0]
    });

    on_dungeon_npc_killed(&ctx, sys_id, w2_npc, char_id).await;
    let w3_npc = read_dungeon_runtime_store(&ctx, |store| {
        let rt = &store.instances[&inst_id];
        assert!(!rt.completed);
        assert!(rt.rooms[0].npc_groups[2].spawned, "Wave 3 optional spawns after Wave 2 kill");
        rt.rooms[0].npc_groups[2].npc_ball_ids[0]
    });

    on_dungeon_npc_killed(&ctx, sys_id, w3_npc, char_id).await;
    read_dungeon_runtime_store(&ctx, |store| {
        let rt = &store.instances[&inst_id];
        assert!(
            !rt.rooms[0].gate_locked && rt.rooms[0].cleared && !rt.completed,
            "Room 1 gate unlocks cleanly only after untriggered Wave 3 clears"
        );
    });
}

#[test]
fn first_combat_wave_with_non_kill_or_compound_trigger_stays_non_immediate() {
    let sde = shared_sde();
    let ctx = make_ctx(Arc::clone(&sde));
    let concrete_npc_tid = pick_concrete_combat_npc_tid(&sde);

    let variants: [(&str, &str); 6] = [
        ("loot_taken", r#"{"kind": "loot_taken", "item_type_id": 34}"#),
        ("room_entered", r#"{"kind": "room_entered", "target_room_index": 1}"#),
        ("ship_approached", r#"{"kind": "ship_approached", "radius_m": 4000}"#),
        ("timed", r#"{"kind": "timed", "time_seconds": 15}"#),
        (
            "compound_and",
            r#"{"kind": "compound_and", "clauses": [{"kind": "room_entered", "target_room_index": 1}, {"kind": "loot_taken"}]}"#,
        ),
        (
            "compound_or",
            r#"{"kind": "compound_or", "clauses": [{"kind": "npc_damaged", "hull_below_pct": 50}, {"kind": "loot_taken"}]}"#,
        ),
    ];

    for (idx, (label, wtr_json)) in variants.iter().enumerate() {
        let did = 991_110_u32 + (idx as u32);
        let payload = format!(
            r#"{{
  "header": {{"schema_version": 2, "source": "EVE University Wiki (CC BY-SA 4.0)", "license": "CC BY-SA 4.0"}},
  "entries": [{{
    "entry_id": "non_kill_first_{label}",
    "base_title": "NonKill {label}",
    "sde_matches": {{"dungeon_ids": [{did}], "mission_ids": []}},
    "rooms": [{{
      "room_index": 1,
      "waves": [
        {{
          "wave_index": 1,
          "timing": "static_structures",
          "spawns": [{{
            "raw_class": "Container",
            "raw_name": "Trigger Container",
            "count": {{"min": 1, "max": 1}},
            "resolution": {{"status": "resolved", "type_ids": [23], "candidate_type_ids": [], "had_ellipsis": false}}
          }}]
        }},
        {{
          "wave_index": 2,
          "timing": "sequential_wave",
          "wave_trigger": {wtr_json},
          "spawns": [{{
            "raw_class": "Frigate",
            "raw_name": "Ambush Wing",
            "count": {{"min": 1, "max": 1}},
            "resolution": {{"status": "resolved_exact", "type_ids": [{concrete_npc_tid}], "candidate_type_ids": [], "had_ellipsis": false}}
          }}]
        }}
      ]
    }}]
  }}]
}}"#
        );
        let cat = Arc::new(parse_and_validate_wiki_pve_json(&payload).expect("valid non-kill wave json"));
        install_wiki_pve_catalog_for_test(&ctx, cat);
        let built = build_wiki_deadspace_rooms(
            &ctx,
            30_000_142,
            DungeonTemplateId::new(did),
            &format!("NonKill {label}"),
            [10_000_000.0, 0.0, 0.0],
            false,
        )
        .expect("builds non-kill wave");
        let r0 = &built.rooms[0];
        assert_eq!(r0.npc_groups.len(), 1);
        assert!(
            !r0.npc_groups[0].spawn_immediately,
            "{label}: first combat wave gated by {label} must remain spawn_immediately == false"
        );
        assert!(
            r0.triggers
                .iter()
                .any(|t| matches!(t.action, DungeonTriggerAction::SpawnNpcGroup(1))),
            "{label}: translated wave_trigger must emit SpawnNpcGroup(1)"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn group_clear_and_class_clear_spawn_triggers_do_not_fire_on_first_kill() {
    let sde = shared_sde();
    let ctx = make_ctx(Arc::clone(&sde));
    let concrete_npc_tid = pick_concrete_combat_npc_tid(&sde);
    let sys_id = 30_000_142_u32;
    let char_id = 90_001_120_i64;

    // Group-clear spawn triggers with importer-assigned trigger_ship_tag must emit OnKill(group)
    // and never collapse to 1-kill OnKillTagged.
    let did_group = 991_120_u32;
    let group_json = format!(
        r#"{{
  "header": {{"schema_version": 2, "source": "EVE University Wiki (CC BY-SA 4.0)", "license": "CC BY-SA 4.0"}},
  "entries": [{{
    "entry_id": "group_clear_spawn_trigger",
    "base_title": "Group Clear Site",
    "sde_matches": {{"dungeon_ids": [{did_group}], "mission_ids": []}},
    "rooms": [{{
      "room_index": 1,
      "waves": [
        {{
          "wave_index": 1,
          "timing": "initial_spawn",
          "spawns": [
            {{
              "raw_class": "Frigate",
              "raw_name": "Twin Escort",
              "tag": "r1_w1_s1",
              "count": {{"min": 2, "max": 2}},
              "resolution": {{"status": "resolved_exact", "type_ids": [{concrete_npc_tid}], "candidate_type_ids": [], "had_ellipsis": false}},
              "trigger": {{
                "kind": "last_in_group_destroyed",
                "trigger_ship_tag": "r1_w1_s1",
                "target_wave_index": 2
              }}
            }},
            {{
              "raw_class": "Cruiser",
              "raw_name": "Extra Wing",
              "tag": "r1_w1_s2",
              "count": {{"min": 1, "max": 1}},
              "resolution": {{"status": "resolved_exact", "type_ids": [{concrete_npc_tid}], "candidate_type_ids": [], "had_ellipsis": false}},
              "trigger": {{
                "kind": "group_destroyed",
                "trigger_ship_tag": "r1_w1_s2",
                "target_wave_index": 2
              }}
            }}
          ]
        }},
        {{
          "wave_index": 2,
          "timing": "sequential_wave",
          "spawns": [{{
            "raw_class": "Battleship",
            "raw_name": "Reinforcement",
            "count": {{"min": 1, "max": 1}},
            "resolution": {{"status": "resolved_exact", "type_ids": [{concrete_npc_tid}], "candidate_type_ids": [], "had_ellipsis": false}}
          }}]
        }}
      ]
    }}]
  }}]
}}"#
    );
    let cat_group = Arc::new(parse_and_validate_wiki_pve_json(&group_json).expect("valid group clear"));
    install_wiki_pve_catalog_for_test(&ctx, cat_group);
    let inst_group = spawn_sde_dungeon_runtime_instance(
        &ctx,
        char_id,
        sys_id,
        did_group,
        "Deadspace",
        [30_000_000.0, 0.0, 0.0],
        false,
    )
    .await
    .expect("spawn group-clear site");

    let group_npcs = read_dungeon_runtime_store(&ctx, |store| {
        let r0 = &store.instances[&inst_group].rooms[0];
        assert!(
            r0.triggers.iter().all(|t| !matches!(t.kind, DungeonTriggerKind::OnKillTagged { .. })),
            "last_in_group_destroyed and group_destroyed must not collapse to OnKillTagged"
        );
        assert!(r0.triggers.iter().any(|t| matches!(
            (&t.kind, &t.action),
            (DungeonTriggerKind::OnKill { target_npc_group: Some(1), .. }, DungeonTriggerAction::SpawnNpcGroup(2))
        )));
        r0.npc_groups[0].npc_ball_ids.clone()
    });
    assert_eq!(group_npcs.len(), 3);
    on_dungeon_npc_killed(&ctx, sys_id, group_npcs[0], char_id).await;
    on_dungeon_npc_killed(&ctx, sys_id, group_npcs[1], char_id).await;
    read_dungeon_runtime_store(&ctx, |store| {
        assert!(
            !store.instances[&inst_group].rooms[0].npc_groups[1].spawned,
            "Wave 2 must NOT spawn while 1 ship remains in Group 1"
        );
    });
    on_dungeon_npc_killed(&ctx, sys_id, group_npcs[2], char_id).await;
    read_dungeon_runtime_store(&ctx, |store| {
        assert!(
            store.instances[&inst_group].rooms[0].npc_groups[1].spawned,
            "Wave 2 spawns once entire Group 1 clears"
        );
    });

    // Class-clear spawn triggers with count > 1 must derive required_kills from materialized class count.
    let did_class = 991_121_u32;
    let class_json = format!(
        r#"{{
  "header": {{"schema_version": 2, "source": "EVE University Wiki (CC BY-SA 4.0)", "license": "CC BY-SA 4.0"}},
  "entries": [{{
    "entry_id": "class_clear_spawn_trigger",
    "base_title": "Class Clear Site",
    "sde_matches": {{"dungeon_ids": [{did_class}], "mission_ids": []}},
    "rooms": [{{
      "room_index": 1,
      "waves": [
        {{
          "wave_index": 1,
          "timing": "initial_spawn",
          "spawns": [
            {{
              "raw_class": "Cruiser",
              "raw_name": "Squad Cruiser",
              "tag": "cruiser_squad",
              "count": {{"min": 3, "max": 3}},
              "resolution": {{"status": "resolved_exact", "type_ids": [{concrete_npc_tid}], "candidate_type_ids": [], "had_ellipsis": false}},
              "trigger": {{
                "kind": "last_of_class_destroyed",
                "trigger_ship_tag": "cruiser_squad",
                "target_wave_index": 2
              }}
            }},
            {{
              "raw_class": "Frigate",
              "raw_name": "Decoy Frigate",
              "tag": "decoy_frigate",
              "count": {{"min": 1, "max": 1}},
              "resolution": {{"status": "resolved_exact", "type_ids": [{concrete_npc_tid}], "candidate_type_ids": [], "had_ellipsis": false}}
            }}
          ]
        }},
        {{
          "wave_index": 2,
          "timing": "sequential_wave",
          "spawns": [{{
            "raw_class": "Battleship",
            "raw_name": "Commander",
            "count": {{"min": 1, "max": 1}},
            "resolution": {{"status": "resolved_exact", "type_ids": [{concrete_npc_tid}], "candidate_type_ids": [], "had_ellipsis": false}}
          }}]
        }}
      ]
    }}]
  }}]
}}"#
    );
    let cat_class = Arc::new(parse_and_validate_wiki_pve_json(&class_json).expect("valid class clear"));
    install_wiki_pve_catalog_for_test(&ctx, cat_class);
    let inst_class = spawn_sde_dungeon_runtime_instance(
        &ctx,
        char_id,
        sys_id,
        did_class,
        "Deadspace",
        [35_000_000.0, 0.0, 0.0],
        false,
    )
    .await
    .expect("spawn class-clear site");

    let (cruiser_ids, decoy_id) = read_dungeon_runtime_store(&ctx, |store| {
        let r0 = &store.instances[&inst_class].rooms[0];
        assert!(
            r0.triggers.iter().any(|t| matches!(
                (&t.kind, &t.action),
                (
                    DungeonTriggerKind::OnKillTagged { target_tag, required_kills: Some(3), .. },
                    DungeonTriggerAction::SpawnNpcGroup(2)
                ) if target_tag == "cruiser_squad"
            )),
            "last_of_class_destroyed on 3 cruisers must set required_kills == Some(3)"
        );
        let g1 = &r0.npc_groups[0];
        let mut cruisers = Vec::new();
        let mut decoy = 0;
        for (&bid, tag) in &g1.npc_ball_tags {
            if tag == "cruiser_squad" {
                cruisers.push(bid);
            } else {
                decoy = bid;
            }
        }
        (cruisers, decoy)
    });
    assert_eq!(cruiser_ids.len(), 3);
    assert!(decoy_id > 0);

    on_dungeon_npc_killed(&ctx, sys_id, decoy_id, char_id).await;
    on_dungeon_npc_killed(&ctx, sys_id, cruiser_ids[0], char_id).await;
    on_dungeon_npc_killed(&ctx, sys_id, cruiser_ids[1], char_id).await;
    read_dungeon_runtime_store(&ctx, |store| {
        assert!(
            !store.instances[&inst_class].rooms[0].npc_groups[1].spawned,
            "Wave 2 must NOT spawn until all 3 tagged cruisers die"
        );
    });
    on_dungeon_npc_killed(&ctx, sys_id, cruiser_ids[2], char_id).await;
    read_dungeon_runtime_store(&ctx, |store| {
        assert!(
            store.instances[&inst_class].rooms[0].npc_groups[1].spawned,
            "Wave 2 spawns on 3rd tagged cruiser kill even while wave isn't cleared prior"
        );
    });
}

#[test]
fn explicit_wave_actions_targeting_unmaterialized_optional_wave_do_not_abort_build() {
    let sde = shared_sde();
    let ctx = make_ctx(Arc::clone(&sde));
    let concrete_npc_tid = pick_concrete_combat_npc_tid(&sde);
    let custom_did = 991_130_u32;

    let payload = format!(
        r#"{{
  "header": {{"schema_version": 2, "source": "EVE University Wiki (CC BY-SA 4.0)", "license": "CC BY-SA 4.0"}},
  "entries": [{{
    "entry_id": "optional_target_wave_action",
    "base_title": "Optional Target Action Complex",
    "sde_matches": {{"dungeon_ids": [{custom_did}], "mission_ids": []}},
    "rooms": [{{
      "room_index": 1,
      "waves": [
        {{
          "wave_index": 1,
          "timing": "initial_spawn",
          "wave_trigger": {{
            "kind": "this_ship_destroyed",
            "action": {{ "kind": "spawn_npc_group", "target_wave_index": 2 }},
            "extra_actions": [
              {{ "kind": "despawn_npc_group", "target_wave_index": 2 }},
              {{ "kind": "grant_reward", "isk": 150000 }}
            ]
          }},
          "spawns": [{{
            "raw_class": "Frigate",
            "count": {{"min": 1, "max": 1}},
            "resolution": {{"status": "resolved_exact", "type_ids": [{concrete_npc_tid}], "candidate_type_ids": [], "had_ellipsis": false}}
          }}]
        }},
        {{
          "wave_index": 2,
          "timing": "optional_spawn",
          "spawn_chance_bps": 0,
          "spawns": [{{
            "raw_class": "Cruiser",
            "count": {{"min": 1, "max": 1}},
            "resolution": {{"status": "resolved_exact", "type_ids": [{concrete_npc_tid}], "candidate_type_ids": [], "had_ellipsis": false}}
          }}]
        }}
      ]
    }}]
  }}]
}}"#
    );
    let cat = Arc::new(parse_and_validate_wiki_pve_json(&payload).expect("valid optional target wave json"));
    install_wiki_pve_catalog_for_test(&ctx, cat);

    let built = build_wiki_deadspace_rooms(
        &ctx,
        30_000_142,
        DungeonTemplateId::new(custom_did),
        "Optional Target Action Complex",
        [10_000_000.0, 0.0, 0.0],
        false,
    )
    .expect("valid optional wave miss must not abort build_wiki_deadspace_rooms to None");
    assert_eq!(built.rooms.len(), 1);
    assert_eq!(built.rooms[0].npc_groups.len(), 1);
    assert!(
        built.rooms[0].triggers.iter().any(|t| matches!(
            (&t.kind, &t.action),
            (
                DungeonTriggerKind::OnKill {
                    target_npc_group: Some(1),
                    ..
                },
                DungeonTriggerAction::GrantReward { isk: 150_000, .. }
            )
        )),
        "surviving sibling action GrantReward must bind to immediate group 1 when optional wave action is omitted"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn non_kill_and_tagged_spawn_unlock_triggers_are_not_bypassed_and_preserve_objectives() {
    use eve_proto::PyRep;
    use std::collections::HashMap;

    let sde = shared_sde();
    let ctx = make_ctx(Arc::clone(&sde));
    let concrete_npc_tid = pick_concrete_combat_npc_tid(&sde);
    let sys_id = 30_000_142_u32;
    let char_id = 90_001_140_i64;
    let did_loot_unlock = 991_140_u32;
    let did_tagged_unlock = 991_141_u32;

    let payload = format!(
        r#"{{
  "header": {{"schema_version": 2, "source": "EVE University Wiki (CC BY-SA 4.0)", "license": "CC BY-SA 4.0"}},
  "entries": [
    {{
      "entry_id": "loot_unlock_no_npc_bypass",
      "base_title": "Loot Unlock Complex",
      "objective": {{
        "objective_id": 99401,
        "key": "loot_keycard",
        "title": "Recover Keycard",
        "kind": "loot_item",
        "target_type_id": 34,
        "room_index": 1
      }},
      "sde_matches": {{"dungeon_ids": [{did_loot_unlock}], "mission_ids": []}},
      "rooms": [
        {{
          "room_index": 1,
          "waves": [{{
            "wave_index": 1,
            "timing": "initial_spawn",
            "spawns": [
              {{
                "raw_class": "Container",
                "raw_name": "Keycard Container",
                "count": {{"min": 1, "max": 1}},
                "resolution": {{"status": "resolved", "type_ids": [23], "candidate_type_ids": [], "had_ellipsis": false}},
                "trigger": {{
                  "kind": "loot_taken",
                  "target_object_index": 0,
                  "item_type_id": 34,
                  "unlocks_gate": true
                }}
              }},
              {{
                "raw_class": "Frigate",
                "raw_name": "Sentry Frigate",
                "count": {{"min": 1, "max": 1}},
                "resolution": {{"status": "resolved_exact", "type_ids": [{concrete_npc_tid}], "candidate_type_ids": [], "had_ellipsis": false}},
                "trigger": {{
                  "kind": "this_ship_destroyed",
                  "trigger_ship_tag": "sentry_1",
                  "target_room_index": 2,
                  "action": {{ "kind": "trigger_message", "message_type": 1, "message_id": 77 }}
                }}
              }}
            ]
          }}]
        }},
        {{
          "room_index": 2,
          "waves": [{{
            "wave_index": 1,
            "timing": "initial_spawn",
            "spawns": [{{
              "raw_class": "Cruiser",
              "raw_name": "Inner Guard",
              "count": {{"min": 1, "max": 1}},
              "resolution": {{"status": "resolved_exact", "type_ids": [{concrete_npc_tid}], "candidate_type_ids": [], "had_ellipsis": false}}
            }}]
          }}]
        }}
      ]
    }},
    {{
      "entry_id": "tagged_unlock_single_trigger",
      "base_title": "Tagged Unlock Complex",
      "sde_matches": {{"dungeon_ids": [{did_tagged_unlock}], "mission_ids": []}},
      "rooms": [
        {{
          "room_index": 1,
          "waves": [{{
            "wave_index": 1,
            "timing": "initial_spawn",
            "spawns": [
              {{
                "raw_class": "Frigate",
                "raw_name": "Gatekeeper",
                "tag": "key_boss",
                "count": {{"min": 1, "max": 1}},
                "resolution": {{"status": "resolved_exact", "type_ids": [{concrete_npc_tid}], "candidate_type_ids": [], "had_ellipsis": false}},
                "trigger": {{
                  "kind": "this_ship_destroyed",
                  "trigger_ship_tag": "key_boss",
                  "unlocks_gate": true
                }}
              }},
              {{
                "raw_class": "Frigate",
                "raw_name": "Escort",
                "count": {{"min": 1, "max": 1}},
                "resolution": {{"status": "resolved_exact", "type_ids": [{concrete_npc_tid}], "candidate_type_ids": [], "had_ellipsis": false}}
              }}
            ]
          }}]
        }},
        {{
          "room_index": 2,
          "waves": [{{
            "wave_index": 1,
            "timing": "initial_spawn",
            "spawns": [{{
              "raw_class": "Cruiser",
              "raw_name": "Vault Keeper",
              "count": {{"min": 1, "max": 1}},
              "resolution": {{"status": "resolved_exact", "type_ids": [{concrete_npc_tid}], "candidate_type_ids": [], "had_ellipsis": false}}
            }}]
          }}]
        }}
      ]
    }},
    {{
      "entry_id": "mixed_case_can_1",
      "rooms": [{{ "room_index": 1, "waves": [{{ "wave_index": 1, "timing": "sequential_wave", "wave_trigger": {{ "kind": "Can_Hacked" }}, "spawns": [] }}] }}]
    }},
    {{
      "entry_id": "mixed_case_can_2",
      "rooms": [{{ "room_index": 1, "waves": [{{ "wave_index": 1, "timing": "sequential_wave", "wave_trigger": {{ "kind": "can_hacked" }}, "spawns": [] }}] }}]
    }}
  ]
}}"#
    );
    let cat = Arc::new(parse_and_validate_wiki_pve_json(&payload).expect("valid BF-05 / P2 payload"));
    assert_eq!(cat.stats.rejected_unsupported_trigger_entries, 2);
    assert_eq!(cat.stats.rejected_by_trigger_kind.get("can_hacked"), Some(&2));
    install_wiki_pve_catalog_for_test(&ctx, cat);

    // spawn_dungeon_instance propagates objectives when rooms argument is omitted.
    let mut kw = HashMap::new();
    kw.insert("dungeonID".to_string(), PyRep::Integer(i64::from(did_loot_unlock)));
    kw.insert("solarSystemID".to_string(), PyRep::Integer(i64::from(sys_id)));
    kw.insert("dungeonName".to_string(), PyRep::String("Loot Unlock Complex".into()));
    let spawned = spawn_dungeon_instance(&ctx, char_id, Some(sys_id), &[], &kw)
        .await
        .expect("spawn_dungeon_instance wiki");
    let inst_loot = spawned.dict_get("instanceID").and_then(|v| v.as_i64()).expect("instanceID");

    let (npc_id, container_id) = read_dungeon_runtime_store(&ctx, |store| {
        let rt = &store.instances[&inst_loot];
        assert_eq!(rt.objectives.len(), 1);
        assert_eq!(rt.objectives[0].objective_id, 99_401);
        assert_eq!(rt.active_objective_id, 99_401);
        let r0 = &rt.rooms[0];
        assert!(r0.gate_locked);
        assert!(
            r0.triggers.iter().any(|t| t.target_room_index == Some(1)
                && matches!(t.action, DungeonTriggerAction::TriggerMessage { message_id: 77, .. })),
            "spawn-level target_room_index: 2 must map to room_vec_idx Some(1)"
        );
        let unlock_trigs: Vec<_> = r0
            .triggers
            .iter()
            .filter(|t| t.all_actions().any(|a| matches!(a, DungeonTriggerAction::UnlockGate)))
            .collect();
        assert_eq!(unlock_trigs.len(), 1);
        assert!(matches!(
            unlock_trigs[0].kind,
            DungeonTriggerKind::OnLoot { item_type_id: Some(34), .. }
        ));
        (r0.npc_groups[0].npc_ball_ids[0], r0.objects[0].ball_id)
    });

    // Killing the sentry NPC must NOT bypass the OnLoot gate unlock trigger.
    on_dungeon_npc_killed(&ctx, sys_id, npc_id, char_id).await;
    read_dungeon_runtime_store(&ctx, |store| {
        assert!(
            store.instances[&inst_loot].rooms[0].gate_locked,
            "OnLoot gate unlock must stay locked after combat NPC death"
        );
    });

    // Looting item type 34 unlocks the gate.
    on_dungeon_item_looted(&ctx, char_id, Some(sys_id), container_id, 34).await;
    read_dungeon_runtime_store(&ctx, |store| {
        assert!(
            !store.instances[&inst_loot].rooms[0].gate_locked,
            "OnLoot item 34 must unlock Room 1 gate"
        );
    });

    // Tagged single-ship unlock emits exactly one OnKillTagged -> UnlockGate trigger (never duplicate OnKill(1)).
    let tagged_build = build_wiki_deadspace_rooms(
        &ctx,
        sys_id,
        DungeonTemplateId::new(did_tagged_unlock),
        "Tagged Unlock Complex",
        [45_000_000.0, 0.0, 0.0],
        false,
    )
    .expect("tagged unlock build");
    let tagged_unlocks: Vec<_> = tagged_build.rooms[0]
        .triggers
        .iter()
        .filter(|t| t.all_actions().any(|a| matches!(a, DungeonTriggerAction::UnlockGate)))
        .collect();
    assert_eq!(tagged_unlocks.len(), 1);
    assert!(matches!(
        &tagged_unlocks[0].kind,
        DungeonTriggerKind::OnKillTagged { target_tag, .. } if target_tag == "key_boss"
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn immediate_wave_triggers_bind_own_group_and_skipped_spawns_do_not_deadlock() {
    use eve_machonet::SessionState;
    use eve_proto::PyRep;

    let sde = shared_sde();
    let ctx = make_ctx(Arc::clone(&sde));
    let concrete_npc_tid = pick_concrete_combat_npc_tid(&sde);
    let sys_id = 30_000_142_u32;
    let char_id = 90_001_150_i64;
    let ship_id = 1_021_150_001_i64;

    // 1. Immediate Wave 1 spawning materialized Wave 2 binds default_src=Some(1) (no all_spawned deadlock),
    // and simultaneous_initial_group Wave 2 binds default_src=Some(2) rather than Wave 1.
    let did_imm_chain = 991_150_u32;
    let did_sim_w2 = 991_151_u32;
    let did_imm_unlocks = 991_152_u32;
    let did_skipped_sp = 991_153_u32;
    let did_obj_prec = 991_154_u32;

    let payload = format!(
        r#"{{
  "header": {{"schema_version": 2, "source": "EVE University Wiki (CC BY-SA 4.0)", "license": "CC BY-SA 4.0"}},
  "entries": [
    {{
      "entry_id": "imm_w1_spawns_w2",
      "base_title": "Immediate Wave Chain",
      "sde_matches": {{"dungeon_ids": [{did_imm_chain}], "mission_ids": []}},
      "rooms": [{{
        "room_index": 1,
        "waves": [
          {{
            "wave_index": 1,
            "timing": "initial_spawn",
            "wave_trigger": {{
              "kind": "this_ship_destroyed",
              "action": {{ "kind": "spawn_npc_group", "target_wave_index": 2 }}
            }},
            "spawns": [{{
              "raw_class": "Frigate",
              "count": {{"min": 1, "max": 1}},
              "resolution": {{"status": "resolved_exact", "type_ids": [{concrete_npc_tid}], "candidate_type_ids": [], "had_ellipsis": false}}
            }}]
          }},
          {{
            "wave_index": 2,
            "timing": "sequential_wave",
            "spawns": [{{
              "raw_class": "Cruiser",
              "count": {{"min": 1, "max": 1}},
              "resolution": {{"status": "resolved_exact", "type_ids": [{concrete_npc_tid}], "candidate_type_ids": [], "had_ellipsis": false}}
            }}]
          }}
        ]
      }}]
    }},
    {{
      "entry_id": "sim_initial_w2_own_group",
      "base_title": "Simultaneous Initial Group",
      "sde_matches": {{"dungeon_ids": [{did_sim_w2}], "mission_ids": []}},
      "rooms": [{{
        "room_index": 1,
        "waves": [
          {{
            "wave_index": 1,
            "timing": "initial_spawn",
            "spawns": [{{
              "raw_class": "Frigate",
              "count": {{"min": 1, "max": 1}},
              "resolution": {{"status": "resolved_exact", "type_ids": [{concrete_npc_tid}], "candidate_type_ids": [], "had_ellipsis": false}}
            }}]
          }},
          {{
            "wave_index": 2,
            "timing": "simultaneous_initial_group",
            "wave_trigger": {{
              "kind": "npc_damaged",
              "hull_below_pct": 60,
              "action": {{ "kind": "trigger_message", "message_type": 1, "message_id": 222 }}
            }},
            "spawns": [{{
              "raw_class": "Cruiser",
              "count": {{"min": 1, "max": 1}},
              "resolution": {{"status": "resolved_exact", "type_ids": [{concrete_npc_tid}], "candidate_type_ids": [], "had_ellipsis": false}}
            }}]
          }}
        ]
      }}]
    }},
    {{
      "entry_id": "imm_wave_triggers_inline_unlock",
      "base_title": "Immediate Wave Triggers Inline Unlock",
      "sde_matches": {{"dungeon_ids": [{did_imm_unlocks}], "mission_ids": []}},
      "rooms": [
        {{
          "room_index": 1,
          "waves": [{{
            "wave_index": 1,
            "timing": "initial_spawn",
            "wave_trigger": {{
              "kind": "npc_damaged",
              "hull_below_pct": 50,
              "trigger_delay_secs": 5,
              "target_room_index": 1,
              "unlocks_gate": true
            }},
            "spawns": [{{
              "raw_class": "Cruiser",
              "count": {{"min": 1, "max": 1}},
              "resolution": {{"status": "resolved_exact", "type_ids": [{concrete_npc_tid}], "candidate_type_ids": [], "had_ellipsis": false}}
            }}]
          }}]
        }},
        {{
          "room_index": 2,
          "waves": [{{
            "wave_index": 1,
            "timing": "initial_spawn",
            "wave_trigger": {{
              "kind": "kill_count",
              "required_kills": 2,
              "unlocks_gate": true
            }},
            "spawns": [{{
              "raw_class": "Frigate",
              "count": {{"min": 3, "max": 3}},
              "resolution": {{"status": "resolved_exact", "type_ids": [{concrete_npc_tid}], "candidate_type_ids": [], "had_ellipsis": false}}
            }}]
          }}]
        }},
        {{
          "room_index": 3,
          "waves": [{{
            "wave_index": 1,
            "timing": "initial_spawn",
            "wave_trigger": {{
              "kind": "kill_tagged",
              "target_tag": "vault_boss",
              "unlocks_gate": true
            }},
            "spawns": [{{
              "raw_class": "Battleship",
              "tag": "vault_boss",
              "count": {{"min": 1, "max": 1}},
              "resolution": {{"status": "resolved_exact", "type_ids": [{concrete_npc_tid}], "candidate_type_ids": [], "had_ellipsis": false}}
            }}]
          }}]
        }}
      ]
    }},
    {{
      "entry_id": "skipped_spawn_no_deadlock_multi_container",
      "base_title": "Skipped Spawn And Second Container",
      "sde_matches": {{"dungeon_ids": [{did_skipped_sp}], "mission_ids": []}},
      "rooms": [
        {{
          "room_index": 1,
          "waves": [{{
            "wave_index": 1,
            "timing": "initial_spawn",
            "spawns": [
              {{
                "raw_class": "Container",
                "raw_name": "First Container",
                "count": {{"min": 1, "max": 1}},
                "resolution": {{"status": "resolved", "type_ids": [23], "candidate_type_ids": [], "had_ellipsis": false}}
              }},
              {{
                "raw_class": "Container",
                "raw_name": "Second Container",
                "count": {{"min": 1, "max": 1}},
                "resolution": {{"status": "resolved", "type_ids": [23], "candidate_type_ids": [], "had_ellipsis": false}},
                "trigger": {{
                  "kind": "loot_taken",
                  "item_type_id": 34,
                  "action": {{ "kind": "trigger_message", "message_type": 1, "message_id": 88 }}
                }}
              }},
              {{
                "raw_class": "Frigate",
                "raw_name": "Skipped Ghost",
                "tag": "ghost_skipped",
                "count": {{"min": 0, "max": 0}},
                "resolution": {{"status": "resolved_exact", "type_ids": [{concrete_npc_tid}], "candidate_type_ids": [], "had_ellipsis": false}},
                "trigger": {{
                  "kind": "this_ship_destroyed",
                  "trigger_ship_tag": "ghost_skipped",
                  "unlocks_gate": true
                }}
              }},
              {{
                "raw_class": "unmapped_weird_tag",
                "raw_name": "Skipped Unresolvable Prop",
                "tag": "unresolved_skipped",
                "count": {{"min": 1, "max": 1}},
                "resolution": {{"status": "unresolved", "type_ids": [], "candidate_type_ids": [], "had_ellipsis": false}},
                "trigger": {{
                  "kind": "this_ship_destroyed",
                  "trigger_ship_tag": "unresolved_skipped",
                  "unlocks_gate": true
                }}
              }},
              {{
                "raw_class": "Frigate",
                "raw_name": "Real Guard",
                "count": {{"min": 1, "max": 1}},
                "resolution": {{"status": "resolved_exact", "type_ids": [{concrete_npc_tid}], "candidate_type_ids": [], "had_ellipsis": false}}
              }}
            ]
          }}]
        }},
        {{
          "room_index": 2,
          "waves": [{{
            "wave_index": 1,
            "timing": "initial_spawn",
            "spawns": [{{
              "raw_class": "Cruiser",
              "count": {{"min": 1, "max": 1}},
              "resolution": {{"status": "resolved_exact", "type_ids": [{concrete_npc_tid}], "candidate_type_ids": [], "had_ellipsis": false}}
            }}]
          }}]
        }}
      ]
    }},
    {{
      "entry_id": "objective_precedence_entry",
      "base_title": "Objective Precedence Complex",
      "objectives": [
        {{ "objective_id": 77100, "key": "global_obj", "title": "Global Objective", "kind": "kill_all" }},
        {{ "objective_id": 77200, "key": "room2_obj", "title": "Clear Inner Pocket", "kind": "kill_all", "room_index": 2 }}
      ],
      "sde_matches": {{"dungeon_ids": [{did_obj_prec}], "mission_ids": []}},
      "rooms": [
        {{
          "room_index": 1,
          "gates": [{{ "target_room_index": 2, "locked": false }}],
          "waves": [{{
            "wave_index": 1,
            "timing": "initial_spawn",
            "spawns": [{{
              "raw_class": "Frigate",
              "count": {{"min": 1, "max": 1}},
              "resolution": {{"status": "resolved_exact", "type_ids": [{concrete_npc_tid}], "candidate_type_ids": [], "had_ellipsis": false}}
            }}]
          }}]
        }},
        {{
          "room_index": 2,
          "waves": [{{
            "wave_index": 1,
            "timing": "initial_spawn",
            "spawns": [{{
              "raw_class": "Cruiser",
              "count": {{"min": 1, "max": 1}},
              "resolution": {{"status": "resolved_exact", "type_ids": [{concrete_npc_tid}], "candidate_type_ids": [], "had_ellipsis": false}}
            }}]
          }}]
        }}
      ]
    }}
  ]
}}"#
    );
    let cat = Arc::new(parse_and_validate_wiki_pve_json(&payload).expect("valid immediate-wave and skipped-spawn json"));
    install_wiki_pve_catalog_for_test(&ctx, cat);

    // Wave 1 immediate wave_trigger binds to Some(1) and spawns Wave 2 live.
    let inst_imm = spawn_sde_dungeon_runtime_instance(
        &ctx,
        char_id,
        sys_id,
        did_imm_chain,
        "Deadspace",
        [60_000_000.0, 0.0, 0.0],
        false,
    )
    .await
    .expect("spawn immediate chain");
    let imm_w1_npc = read_dungeon_runtime_store(&ctx, |store| {
        let r0 = &store.instances[&inst_imm].rooms[0];
        assert!(r0.triggers.iter().any(|t| matches!(
            (&t.kind, &t.action),
            (DungeonTriggerKind::OnKill { target_npc_group: Some(1), .. }, DungeonTriggerAction::SpawnNpcGroup(2))
        )));
        r0.npc_groups[0].npc_ball_ids[0]
    });
    on_dungeon_npc_killed(&ctx, sys_id, imm_w1_npc, char_id).await;
    let imm_w2_npc = read_dungeon_runtime_store(&ctx, |store| {
        let rt = &store.instances[&inst_imm];
        assert!(rt.rooms[0].npc_groups[1].spawned, "Wave 2 spawns on Wave 1 kill without all_spawned deadlock");
        assert!(!rt.completed);
        rt.rooms[0].npc_groups[1].npc_ball_ids[0]
    });
    on_dungeon_npc_killed(&ctx, sys_id, imm_w2_npc, char_id).await;
    read_dungeon_runtime_store(&ctx, |store| {
        assert!(store.instances[&inst_imm].completed);
    });

    // Simultaneous initial Wave 2 binds default_src=Some(2) instead of Some(1).
    let sim_build = build_wiki_deadspace_rooms(
        &ctx,
        sys_id,
        DungeonTemplateId::new(did_sim_w2),
        "Simultaneous Initial Group",
        [10_000_000.0, 0.0, 0.0],
        false,
    )
    .expect("simultaneous build");
    assert!(sim_build.rooms[0].triggers.iter().any(|t| matches!(
        (&t.kind, &t.action),
        (
            DungeonTriggerKind::OnNpcDamaged { target_npc_group: Some(2), hull_or_shield_below_pct: 60, .. },
            DungeonTriggerAction::TriggerMessage { message_id: 222, .. }
        )
    )));

    // Immediate waves with unlocks_gate preserve OnNpcDamaged, OnKillCount, and OnKillTagged.
    let imm_unlock_build = build_wiki_deadspace_rooms(
        &ctx,
        sys_id,
        DungeonTemplateId::new(did_imm_unlocks),
        "Immediate Wave Triggers Inline Unlock",
        [10_000_000.0, 0.0, 0.0],
        false,
    )
    .expect("immediate unlock build");
    let u_r0 = &imm_unlock_build.rooms[0];
    assert!(u_r0.triggers.iter().all(|t| !matches!(t.kind, DungeonTriggerKind::OnKill { .. })));
    assert!(u_r0.triggers.iter().any(|t| {
        matches!(t.kind, DungeonTriggerKind::OnNpcDamaged { target_npc_group: Some(1), hull_or_shield_below_pct: 50, .. })
            && t.action == DungeonTriggerAction::UnlockGate
            && t.delay_secs == 5
            && t.target_room_index == Some(0)
    }));
    let u_r1 = &imm_unlock_build.rooms[1];
    assert!(u_r1.triggers.iter().all(|t| !matches!(t.kind, DungeonTriggerKind::OnKill { .. })));
    assert!(u_r1.triggers.iter().any(|t| {
        matches!(t.kind, DungeonTriggerKind::OnKillCount { target_npc_group: Some(1), required_kills: 2 })
            && t.action == DungeonTriggerAction::UnlockGate
    }));
    let u_r2 = &imm_unlock_build.rooms[2];
    assert!(u_r2.triggers.iter().all(|t| !matches!(t.kind, DungeonTriggerKind::OnKill { .. })));
    assert!(u_r2.triggers.iter().any(|t| {
        matches!(&t.kind, DungeonTriggerKind::OnKillTagged { target_tag, .. } if target_tag == "vault_boss")
            && t.action == DungeonTriggerAction::CompleteDungeon
    }));

    // Verify skipped 0-count / unresolvable spawns emit no unfireable triggers and second container
    // defaults target_object_index to its own materialized object slot.
    let inst_skip = spawn_sde_dungeon_runtime_instance(
        &ctx,
        char_id,
        sys_id,
        did_skipped_sp,
        "Deadspace",
        [65_000_000.0, 0.0, 0.0],
        false,
    )
    .await
    .expect("spawn skipped-spawn dungeon");
    let (guard_id, second_container_id) = read_dungeon_runtime_store(&ctx, |store| {
        let r0 = &store.instances[&inst_skip].rooms[0];
        assert!(
            r0.triggers.iter().all(|t| !matches!(
                &t.kind,
                DungeonTriggerKind::OnKillTagged { target_tag, .. }
                    if target_tag == "ghost_skipped" || target_tag == "unresolved_skipped"
            )),
            "skipped 0-count and unresolvable spawns must not emit unfireable OnKillTagged triggers"
        );
        assert!(
            r0.triggers.iter().any(|t| matches!(
                (&t.kind, &t.action),
                (DungeonTriggerKind::OnLoot { target_object_index: Some(1), item_type_id: Some(34), .. }, DungeonTriggerAction::TriggerMessage { message_id: 88, .. })
            )),
            "second container loot_taken without explicit target_object_index must bind to object index 1"
        );
        (r0.npc_groups[0].npc_ball_ids[0], r0.objects[1].ball_id)
    });
    on_dungeon_npc_killed(&ctx, sys_id, guard_id, char_id).await;
    read_dungeon_runtime_store(&ctx, |store| {
        let r0 = &store.instances[&inst_skip].rooms[0];
        assert!(
            !r0.gate_locked,
            "room with skipped spawn triggers must still unlock gate on real NPC kill"
        );
    });
    on_dungeon_item_looted(&ctx, char_id, Some(sys_id), second_container_id, 34).await;
    read_dungeon_runtime_store(&ctx, |store| {
        let r0 = &store.instances[&inst_skip].rooms[0];
        assert!(
            r0.cleared && !r0.gate_locked,
            "room with skipped spawn triggers and second-container loot trigger must clear cleanly once looted"
        );
    });

    // Check objective room-index precedence over earlier dungeon-wide objective across gate transition.
    ctx.db
        .provision_gm_account("obj_prec_runner", "pw", char_id, "ObjPrecRunner");
    let uid = ctx.db.with_snapshot(|s| s.characters[&char_id].user_id);
    ctx.db
        .acquire_online_lock_with_computer_hash(
            uid,
            char_id as u64,
            Some(char_id),
            None,
            true,
            1_776_400_000,
        )
        .expect("online lock");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship_id;
        }
    });
    let inst_prec = spawn_sde_dungeon_runtime_instance(
        &ctx,
        char_id,
        sys_id,
        did_obj_prec,
        "Deadspace",
        [70_000_000.0, 0.0, 0.0],
        false,
    )
    .await
    .expect("spawn objective precedence dungeon");
    let (link_gate_id, link_gate_pos) = read_dungeon_runtime_store(&ctx, |store| {
        let r0 = &store.instances[&inst_prec].rooms[0];
        (r0.gate_links[0].gate_ball_id.expect("gate materialized"), r0.gate_links[0].gate_position)
    });
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    sys_handle
        .spawn_ball(eve_destiny::Ball {
            id: ship_id,
            type_id: 587,
            position: glam::DVec3::from_array(link_gate_pos),
            velocity: glam::DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        })
        .await
        .expect("spawn ship");
    let mut sess = SessionState::new(930);
    sess.user_id = Some(uid);
    sess.character_id = Some(char_id);
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);
    let gate_res = try_activate_dungeon_runtime_gate(&ctx, &mut sess, sys_id, &[PyRep::Integer(link_gate_id)])
        .await
        .expect("gate handled")
        .expect("gate succeeded");
    assert_eq!(gate_res.dict_get("roomIndex").and_then(|v| v.as_i64()), Some(2));
    read_dungeon_runtime_store(&ctx, |store| {
        assert_eq!(
            store.instances[&inst_prec].active_objective_id, 77_200,
            "room-specific objective (room_index: 2) must take precedence over earlier global objective"
        );
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn synthetic_raid_destructible_no_frigate_takes_damage_and_unlocks_gate() {
    let raw = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/synthetic_wiki_pve_content.json"
    ))
    .expect("read synthetic_wiki_pve_content.json fixture");
    let cat = Arc::new(parse_and_validate_wiki_pve_json(&raw).expect("fixture validates"));
    let sde = shared_sde();
    let ctx = make_ctx(Arc::clone(&sde));
    install_wiki_pve_catalog_for_test(&ctx, cat);

    let sys_id = 30_000_142_u32;
    let char_id = 90_001_501_i64;
    let custom_did = 990_772_u32;
    ctx.db
        .provision_gm_account("bf01_raid_runner", "pw", char_id, "Bf01RaidRunner");
    let (uid, ship_id) = ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.solar_system_id = sys_id;
        ch.station_id = None;
        ch.structure_id = None;
        (ch.user_id, ch.active_ship_id)
    });
    ctx.db
        .acquire_online_lock_with_computer_hash(
            uid,
            char_id as u64,
            Some(char_id),
            None,
            true,
            1_776_400_000,
        )
        .expect("online lock");

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    sys_handle
        .spawn_ball(eve_destiny::Ball {
            id: ship_id,
            type_id: 587,
            position: glam::DVec3::new(90_000_000.0, 0.0, 0.0),
            velocity: glam::DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        })
        .await
        .expect("spawn player ball");

    let mut kwargs = std::collections::HashMap::new();
    kwargs.insert("dungeonID".to_string(), eve_proto::PyRep::Integer(custom_did as i64));
    kwargs.insert(
        "dungeonName".to_string(),
        eve_proto::PyRep::String("Synthetic Raid".to_string()),
    );
    kwargs.insert("hasEntryGate".to_string(), eve_proto::PyRep::Bool(false));
    kwargs.insert(
        "position".to_string(),
        eve_proto::PyRep::Tuple(vec![
            eve_proto::PyRep::Real(90_000_000.0),
            eve_proto::PyRep::Real(0.0),
            eve_proto::PyRep::Real(0.0),
        ]),
    );
    let spawn_res = spawn_dungeon_instance(&ctx, char_id, Some(sys_id), &[], &kwargs)
        .await
        .expect("spawn Synthetic Raid#1");
    let inst_id = spawn_res
        .dict_get("instanceID")
        .and_then(|v| v.as_i64())
        .expect("instanceID");

    let w1_npcs = read_dungeon_runtime_store(&ctx, |store| {
        let r0 = &store.instances[&inst_id].rooms[0];
        assert!(r0.gate_locked, "Room 1 gate must start locked");
        assert!(
            r0.npc_groups[0].npc_entries.iter().all(|e| e.destructible.is_none()),
            "CombatNpc entries must never inherit destructible=Some(false)"
        );
        r0.npc_groups[0].npc_ball_ids.clone()
    });
    assert!(w1_npcs.len() >= 5, "Wave 1 must materialize all combat ships");

    {
        let view = sys_handle.view();
        for &npc_id in &w1_npcs {
            let cs = view.combat_states.get(&npc_id).expect("combat state present");
            assert!(
                !cs.is_tethered,
                "Combat NPC {npc_id} (including destructible: No wiki frigate) must not spawn tethered/invulnerable"
            );
        }
    }

    let lethal = eve_destiny::DamageVector {
        em: 500_000.0,
        thermal: 500_000.0,
        kinetic: 500_000.0,
        explosive: 500_000.0,
    };
    for npc_id in w1_npcs {
        sys_handle
            .apply_damage_with_attacker(npc_id, lethal, Some(char_id))
            .await
            .expect("Combat NPC including destructible: No frigate must accept lethal damage");
    }

    read_dungeon_runtime_store(&ctx, |store| {
        let r0 = &store.instances[&inst_id].rooms[0];
        assert!(
            r0.npc_groups[0].npc_ball_ids.is_empty(),
            "Wave 1 must clear completely"
        );
        assert!(
            r0.npc_groups[1].spawned,
            "Clearing Wave 1 must spawn Wave 2 without deadlocking on invulnerable frigate"
        );
        assert!(
            !r0.gate_locked && r0.gate_links.iter().all(|gl| !gl.locked),
            "Destroying Wave 1 r1_w1_s4 frigate must unlock Room 1 gate"
        );
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unclassified_triggers_non_combat_locked_gates_and_p2_wave_room_tag_objective_fixes() {
    let sde = shared_sde();
    let ctx = make_ctx(Arc::clone(&sde));
    let concrete_npc_tid = pick_concrete_combat_npc_tid(&sde);
    let sys_id = 30_000_142_u32;

    // 1. BF-02: Importer unclassified triggers (kind: null, classification_state: "unclassified", condition: null)
    // and bare kind: null, condition: null with original_text must reject and increment rejected_by_trigger_kind.
    let bad_unclassified_json = format!(
        r#"{{
  "header": {{"schema_version": 2, "source": "EVE University Wiki (CC BY-SA 4.0)", "license": "CC BY-SA 4.0"}},
  "indexes": {{
    "by_dungeon_id": {{
      "991501": ["unclassified_wave_entry"],
      "991502": ["unclassified_bare_entry"],
      "991503": ["valid_survivor_entry"]
    }}
  }},
  "entries": [
    {{
      "entry_id": "unclassified_wave_entry",
      "base_title": "Unclassified Wave Site",
      "sde_matches": {{"dungeon_ids": [991501], "mission_ids": []}},
      "rooms": [{{
        "room_index": 1,
        "waves": [
          {{
            "wave_index": 1,
            "timing": "initial_spawn",
            "spawns": [{{
              "raw_class": "Frigate",
              "count": {{"min": 1, "max": 1}},
              "resolution": {{"status": "resolved_exact", "type_ids": [{concrete_npc_tid}], "candidate_type_ids": [], "had_ellipsis": false}}
            }}]
          }},
          {{
            "wave_index": 2,
            "timing": "sequential_wave",
            "wave_trigger": {{
              "kind": null,
              "classification_state": "unclassified",
              "original_text": "possible reinforcement?",
              "condition": null
            }},
            "spawns": [{{
              "raw_class": "Cruiser",
              "count": {{"min": 1, "max": 1}},
              "resolution": {{"status": "resolved_exact", "type_ids": [{concrete_npc_tid}], "candidate_type_ids": [], "had_ellipsis": false}}
            }}]
          }}
        ]
      }}]
    }},
    {{
      "entry_id": "unclassified_bare_entry",
      "base_title": "Bare Null Trigger Site",
      "sde_matches": {{"dungeon_ids": [991502], "mission_ids": []}},
      "rooms": [{{
        "room_index": 1,
        "waves": [{{
          "wave_index": 1,
          "timing": "initial_spawn",
          "spawns": [{{
            "raw_class": "Frigate",
            "count": {{"min": 1, "max": 1}},
            "resolution": {{"status": "resolved_exact", "type_ids": [{concrete_npc_tid}], "candidate_type_ids": [], "had_ellipsis": false}},
            "trigger": {{
              "kind": null,
              "condition": null,
              "original_text": "may trigger wave?"
            }}
          }}]
        }}]
      }}]
    }},
    {{
      "entry_id": "valid_survivor_entry",
      "base_title": "Valid Survivor",
      "sde_matches": {{"dungeon_ids": [991503], "mission_ids": []}},
      "rooms": [{{
        "room_index": 1,
        "waves": [{{
          "wave_index": 1,
          "timing": "initial_spawn",
          "spawns": [{{
            "raw_class": "Frigate",
            "count": {{"min": 1, "max": 1}},
            "resolution": {{"status": "resolved_exact", "type_ids": [{concrete_npc_tid}], "candidate_type_ids": [], "had_ellipsis": false}}
          }}]
        }}]
      }}]
    }}
  ]
}}"#
    );
    let cat_uncl = parse_and_validate_wiki_pve_json(&bad_unclassified_json).expect("valid wrapper JSON");
    assert_eq!(cat_uncl.entries.len(), 1);
    assert_eq!(cat_uncl.entries[0].entry_id, "valid_survivor_entry");
    assert_eq!(cat_uncl.stats.rejected_unsupported_trigger_entries, 2);
    assert_eq!(cat_uncl.stats.rejected_by_trigger_kind.get("unclassified").copied(), Some(2));
    assert!(!cat_uncl.by_dungeon_id.contains_key(&991_501));
    assert!(!cat_uncl.by_dungeon_id.contains_key(&991_502));

    let empty_w = std::collections::HashMap::new();
    let empty_r = std::collections::HashMap::new();
    let direct_uncl = WikiTrigger {
        kind: None,
        classification_state: Some("Unclassified".to_string()),
        original_text: Some("unconfirmed".to_string()),
        ..Default::default()
    };
    assert!(translate_wiki_trigger_kind(
        &direct_uncl,
        None,
        [0.0; 3],
        &[],
        &empty_w,
        &empty_r,
        None,
        false
    )
    .unwrap_err()
    .contains("unclassified"));

    // P2-3: target_room_index: Some(0) is rejected rather than silently folding to room index 0.
    let bad_zero_room_tr = WikiTrigger {
        kind: Some("room_entered".to_string()),
        target_room_index: Some(0),
        ..Default::default()
    };
    assert!(translate_wiki_trigger_kind(
        &bad_zero_room_tr,
        None,
        [0.0; 3],
        &[],
        &empty_w,
        &empty_r,
        None,
        false
    )
    .unwrap_err()
    .contains("target_room_index"));

    let bad_zero_room_json = format!(
        r#"{{
  "header": {{"schema_version": 2, "source": "EVE University Wiki (CC BY-SA 4.0)", "license": "CC BY-SA 4.0"}},
  "entries": [{{
    "entry_id": "zero_room_idx",
    "rooms": [{{
      "room_index": 1,
      "waves": [{{
        "wave_index": 1,
        "timing": "initial_spawn",
        "wave_trigger": {{ "kind": "this_ship_destroyed", "target_room_index": 0, "unlocks_gate": true }},
        "spawns": [{{
          "raw_class": "Frigate",
          "resolution": {{"status": "resolved_exact", "type_ids": [{concrete_npc_tid}], "candidate_type_ids": [], "had_ellipsis": false}}
        }}]
      }}]
    }}]
  }}]
}}"#
    );
    assert!(parse_and_validate_wiki_pve_json(&bad_zero_room_json)
        .unwrap_err()
        .contains("target_room_index"));

    // 2. BF-03 (0-NPC room with UnlockGate starts gate_locked=true & gate_links[0].locked=true),
    // P2-2 (wave_trigger target_wave_index/target_wave_indices), P2-4 (case-insensitive tag meta),
    // and P2-1 (Room 1 objective precedence across spawn_sde, spawn_dungeon_instance, and op spawn).
    let did_noncombat = 991_510_u32;
    let did_wave_targets = 991_511_u32;
    let did_obj_init = 991_512_u32;
    let combined_json = format!(
        r#"{{
  "header": {{"schema_version": 2, "source": "EVE University Wiki (CC BY-SA 4.0)", "license": "CC BY-SA 4.0"}},
  "entries": [
    {{
      "entry_id": "noncombat_loot_gate_entry",
      "base_title": "Noncombat Puzzle Pocket",
      "sde_matches": {{"dungeon_ids": [{did_noncombat}], "mission_ids": []}},
      "rooms": [
        {{
          "room_index": 1,
          "room_name": "Cache Entry",
          "has_acceleration_gate": true,
          "waves": [{{
            "wave_index": 1,
            "timing": "initial_spawn",
            "spawns": [{{
              "raw_class": "Container",
              "raw_name": "Gate Key Cache",
              "count": {{"min": 1, "max": 1}},
              "resolution": {{"status": "resolved", "type_ids": [23], "candidate_type_ids": [], "had_ellipsis": false}},
              "trigger": {{
                "kind": "loot_taken",
                "item_type_id": 34,
                "unlocks_gate": true
              }}
            }}]
          }}]
        }},
        {{
          "room_index": 2,
          "room_name": "Final Shrine",
          "has_acceleration_gate": false,
          "waves": [{{
            "wave_index": 1,
            "timing": "initial_spawn",
            "spawns": [{{
              "raw_class": "Frigate",
              "count": {{"min": 1, "max": 1}},
              "resolution": {{"status": "resolved_exact", "type_ids": [{concrete_npc_tid}], "candidate_type_ids": [], "had_ellipsis": false}}
            }}]
          }}]
        }}
      ]
    }},
    {{
      "entry_id": "wave_targets_and_case_tag_entry",
      "base_title": "Wave Targets And Tag Case",
      "sde_matches": {{"dungeon_ids": [{did_wave_targets}], "mission_ids": []}},
      "rooms": [{{
        "room_index": 1,
        "waves": [
          {{
            "wave_index": 1,
            "timing": "initial_spawn",
            "wave_trigger": {{
              "kind": "last_of_class_destroyed",
              "target_tag": "boss_escort",
              "target_wave_indices": [2, 3]
            }},
            "spawns": [{{
              "raw_class": "Frigate",
              "raw_name": "Mixed Tag Frigate",
              "tag": "Boss_Escort",
              "count": {{"min": 2, "max": 2}},
              "resolution": {{"status": "resolved_exact", "type_ids": [{concrete_npc_tid}], "candidate_type_ids": [], "had_ellipsis": false}}
            }}]
          }},
          {{
            "wave_index": 2,
            "timing": "sequential_wave",
            "wave_trigger": {{
              "kind": "wave_cleared",
              "source_wave_index": 1,
              "target_wave_index": 3
            }},
            "spawns": [{{
              "raw_class": "Cruiser",
              "count": {{"min": 1, "max": 1}},
              "resolution": {{"status": "resolved_exact", "type_ids": [{concrete_npc_tid}], "candidate_type_ids": [], "had_ellipsis": false}}
            }}]
          }},
          {{
            "wave_index": 3,
            "timing": "optional_spawn",
            "spawns": [{{
              "raw_class": "Battleship",
              "count": {{"min": 1, "max": 1}},
              "resolution": {{"status": "resolved_exact", "type_ids": [{concrete_npc_tid}], "candidate_type_ids": [], "had_ellipsis": false}}
            }}]
          }}
        ]
      }}]
    }},
    {{
      "entry_id": "init_obj_precedence_entry",
      "base_title": "Initial Objective Precedence",
      "objectives": [
        {{ "objective_id": 88100, "key": "global_first", "title": "Global Objective", "kind": "kill_all" }},
        {{ "objective_id": 88101, "key": "room1_second", "title": "Room One Specific", "kind": "kill_all", "room_index": 1 }},
        {{ "objective_id": 88102, "key": "room2_third", "title": "Room Two Specific", "kind": "kill_all", "room_index": 2 }}
      ],
      "sde_matches": {{"dungeon_ids": [{did_obj_init}], "mission_ids": []}},
      "rooms": [
        {{
          "room_index": 1,
          "waves": [{{
            "wave_index": 1,
            "timing": "initial_spawn",
            "spawns": [{{
              "raw_class": "Frigate",
              "count": {{"min": 1, "max": 1}},
              "resolution": {{"status": "resolved_exact", "type_ids": [{concrete_npc_tid}], "candidate_type_ids": [], "had_ellipsis": false}}
            }}]
          }}]
        }},
        {{
          "room_index": 2,
          "waves": [{{
            "wave_index": 1,
            "timing": "initial_spawn",
            "spawns": [{{
              "raw_class": "Cruiser",
              "count": {{"min": 1, "max": 1}},
              "resolution": {{"status": "resolved_exact", "type_ids": [{concrete_npc_tid}], "candidate_type_ids": [], "had_ellipsis": false}}
            }}]
          }}]
        }}
      ]
    }}
  ]
}}"#
    );
    let cat_ok = Arc::new(parse_and_validate_wiki_pve_json(&combined_json).expect("valid combined JSON"));
    install_wiki_pve_catalog_for_test(&ctx, cat_ok);

    // BF-03 live check: 0-NPC room starts locked and unlocks only after container loot.
    let char_nc = 90_001_510_i64;
    let inst_nc = spawn_sde_dungeon_runtime_instance(
        &ctx,
        char_nc,
        sys_id,
        did_noncombat,
        "Deadspace",
        [95_000_000.0, 0.0, 0.0],
        false,
    )
    .await
    .expect("spawn noncombat dungeon");
    let cont_id = read_dungeon_runtime_store(&ctx, |store| {
        let r0 = &store.instances[&inst_nc].rooms[0];
        assert!(r0.npc_groups.is_empty());
        assert!(r0.gate_locked, "0-NPC room with UnlockGate trigger must start gate_locked=true");
        assert_eq!(r0.gate_links.len(), 1);
        assert!(r0.gate_links[0].locked, "synthesized gate_links[0] must start locked=true");
        r0.objects[0].ball_id
    });
    on_dungeon_item_looted(&ctx, char_nc, Some(sys_id), cont_id, 34).await;
    read_dungeon_runtime_store(&ctx, |store| {
        let r0 = &store.instances[&inst_nc].rooms[0];
        assert!(!r0.gate_locked && !r0.gate_links[0].locked, "looting Gate Key Cache must unlock Room 1 gate");
    });

    // P2-2 + P2-4 build check: immediate wave target_wave_indices + sequential wave target_wave_index
    // + case-insensitive Boss_Escort -> boss_escort required_kills: Some(2).
    let wt_build = build_wiki_deadspace_rooms(
        &ctx,
        sys_id,
        DungeonTemplateId::new(did_wave_targets),
        "Wave Targets And Tag Case",
        [96_000_000.0, 0.0, 0.0],
        false,
    )
    .expect("build wave_targets_and_case_tag_entry");
    let r0_wt = &wt_build.rooms[0];
    assert!(r0_wt.triggers.iter().any(|t| {
        matches!(
            &t.kind,
            DungeonTriggerKind::OnKillTagged { target_tag, required_kills: Some(2), .. }
                if target_tag == "boss_escort"
        ) && t.all_actions().any(|a| *a == DungeonTriggerAction::SpawnNpcGroup(2))
            && t.all_actions().any(|a| *a == DungeonTriggerAction::SpawnNpcGroup(3))
    }));
    assert!(r0_wt.triggers.iter().any(|t| {
        matches!(&t.kind, DungeonTriggerKind::OnKill { target_npc_group: Some(1), .. })
            && t.all_actions().any(|a| *a == DungeonTriggerAction::SpawnNpcGroup(2))
            && t.all_actions().any(|a| *a == DungeonTriggerAction::SpawnNpcGroup(3))
    }));

    // P2-1 initial active_objective_id precedence on both spawn_sde and register_sde_operation.
    let char_obj = 90_001_512_i64;
    let inst_sde = spawn_sde_dungeon_runtime_instance(
        &ctx,
        char_obj,
        sys_id,
        did_obj_init,
        "Deadspace",
        [97_000_000.0, 0.0, 0.0],
        false,
    )
    .await
    .expect("spawn init_obj sde dungeon");
    let inst_op = crate::services::pve_dungeon_runtime_rpc::register_sde_operation_dungeon_instance(
        &ctx,
        char_obj + 1,
        30_000_144,
        did_obj_init,
    );
    let mut sp_kwargs = std::collections::HashMap::new();
    sp_kwargs.insert("dungeonID".to_string(), eve_proto::PyRep::Integer(did_obj_init as i64));
    sp_kwargs.insert(
        "position".to_string(),
        eve_proto::PyRep::Tuple(vec![
            eve_proto::PyRep::Real(99_000_000.0),
            eve_proto::PyRep::Real(0.0),
            eve_proto::PyRep::Real(0.0),
        ]),
    );
    let sp_res = spawn_dungeon_instance(&ctx, char_obj + 2, Some(sys_id), &[], &sp_kwargs)
        .await
        .expect("spawn init_obj direct dungeon");
    let inst_sp = sp_res.dict_get("instanceID").and_then(|v| v.as_i64()).expect("instanceID");
    read_dungeon_runtime_store(&ctx, |store| {
        assert_eq!(store.instances[&inst_sde].active_objective_id, 88_101);
        assert_eq!(store.instances[&inst_op].active_objective_id, 88_101);
        assert_eq!(store.instances[&inst_sp].active_objective_id, 88_101);
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cross_room_scoping_out_of_bounds_rejection_and_unclassified_objectives() {
    let sde = shared_sde();
    let ctx = make_ctx(Arc::clone(&sde));
    let concrete_npc_tid = pick_concrete_combat_npc_tid(&sde);
    let sys_id = 30_000_142_u32;

    // 1. Out-of-bounds target_room_index and non-contiguous OnRoomEntered reject cleanly.
    let oob_action_json = format!(
        r#"{{"header":{{"schema_version":2,"source":"test","license":"CC BY-SA 4.0"}},"entries":[{{"entry_id":"oob_act","rooms":[{{"room_index":1,"waves":[{{"wave_index":1,"timing":"initial_spawn","spawns":[{{"raw_class":"Frigate","resolution":{{"status":"resolved_exact","type_ids":[{concrete_npc_tid}]}}}}],"wave_trigger":{{"kind":"this_ship_destroyed","target_room_index":3,"unlocks_gate":true}}}}]}},{{"room_index":2,"waves":[]}}]}}]}}"#
    );
    assert!(parse_and_validate_wiki_pve_json(&oob_action_json)
        .unwrap_err()
        .contains("target_room_index 3"));

    let noncontig_bad_json = r#"{"header":{"schema_version":2,"source":"test","license":"CC BY-SA 4.0"},"entries":[{"entry_id":"noncontig_bad","rooms":[{"room_index":2,"waves":[]},{"room_index":5,"waves":[{"wave_index":1,"timing":"sequential_wave","wave_trigger":{"kind":"room_entered","target_room_index":1},"spawns":[]}]}]}]}"#;
    assert!(parse_and_validate_wiki_pve_json(noncontig_bad_json)
        .unwrap_err()
        .contains("target_room_index 1"));

    // Whitespace-only trigger_ship_tag with bare null kind/condition rejects as unclassified.
    let ws_tag_json = format!(
        r#"{{"header":{{"schema_version":2,"source":"test","license":"CC BY-SA 4.0"}},"entries":[{{"entry_id":"ws_tag","rooms":[{{"room_index":1,"waves":[{{"wave_index":1,"timing":"initial_spawn","spawns":[{{"raw_class":"Frigate","resolution":{{"status":"resolved_exact","type_ids":[{concrete_npc_tid}]}},"trigger":{{"kind":null,"condition":null,"trigger_ship_tag":"   "}}}}]}}]}}]}}]}}"#
    );
    let cat_ws = parse_and_validate_wiki_pve_json(&ws_tag_json).expect("wrapper valid");
    assert_eq!(cat_ws.stats.rejected_by_trigger_kind.get("unclassified").copied(), Some(1));

    // Programmatic catalog bypass on out-of-bounds target_room_index still fails closed in build.
    let prog_oob_did = 991_601_u32;
    let prog_entry = WikiPveEntry {
        entry_id: "prog_oob_room".into(),
        base_title: "Prog OOB Room".into(),
        sde_matches: WikiSdeMatches { dungeon_ids: vec![prog_oob_did], mission_ids: Vec::new() },
        rooms: vec![WikiRoom {
            room_index: 1,
            room_name: "Only".into(),
            has_acceleration_gate: false,
            gates: Vec::new(),
            waves: vec![WikiWave {
                wave_index: 1,
                timing: "initial_spawn".into(),
                wave_trigger: Some(WikiTrigger {
                    kind: Some("room_entered".into()),
                    target_room_index: Some(9),
                    unlocks_gate: Some(true),
                    ..Default::default()
                }),
                spawns: Vec::new(),
                ..Default::default()
            }],
        }],
        ..Default::default()
    };
    let mut prog_by_did = std::collections::BTreeMap::new();
    prog_by_did.insert(prog_oob_did, vec![0]);
    install_wiki_pve_catalog_for_test(&ctx, Arc::new(WikiPveCatalog {
        source: "test".into(),
        license: "CC BY-SA 4.0".into(),
        entries: vec![prog_entry],
        by_entry_id: [("prog_oob_room".into(), 0)].into_iter().collect(),
        by_dungeon_id: prog_by_did,
        by_site_name: std::collections::HashMap::new(),
        stats: WikiPveLoadStats::default(),
    }));
    assert!(build_wiki_deadspace_rooms(&ctx, sys_id, DungeonTemplateId::new(prog_oob_did), "Prog OOB Room", [0.0; 3], false).is_none());

    // 2. F-01 cross-room UnlockGate / SpawnObject / SpawnNpcGroup + non-contiguous room_index + F-02 unclassified objective.
    let did_cross = 991_602_u32;
    let did_combat_cross = 991_603_u32;
    let did_noncontig_ok = 991_604_u32;
    let did_uncl_obj = 991_605_u32;
    let combined_json = format!(
        r#"{{
  "header": {{"schema_version":2,"source":"EVE University Wiki (CC BY-SA 4.0)","license":"CC BY-SA 4.0"}},
  "entries": [
    {{
      "entry_id": "cross_room_3room",
      "base_title": "Cross Room Complex",
      "sde_matches": {{"dungeon_ids":[{did_cross}],"mission_ids":[]}},
      "rooms": [
        {{
          "room_index": 1,
          "waves": [{{
            "wave_index": 1,
            "timing": "initial_spawn",
            "spawns": [{{
              "raw_class": "Container",
              "raw_name": "R1 Remote Console",
              "count": {{"min":1,"max":1}},
              "resolution": {{"status":"resolved","type_ids":[23]}},
              "trigger": {{
                "kind": "loot_taken",
                "item_type_id": 34,
                "target_room_index": 2,
                "unlocks_gate": true,
                "action": {{ "kind": "spawn_object", "object_index": 0 }},
                "extra_actions": [{{ "kind": "spawn_npc_group", "target_wave_index": 2 }}]
              }}
            }}]
          }}]
        }},
        {{
          "room_index": 2,
          "waves": [
            {{
              "wave_index": 1,
              "timing": "initial_spawn",
              "spawns": [
                {{ "raw_class": "Container", "raw_name": "R2 Hidden Cache", "count": {{"min":1,"max":1}}, "resolution": {{"status":"resolved","type_ids":[23]}} }},
                {{ "raw_class": "Frigate", "raw_name": "R2 Guard", "count": {{"min":1,"max":1}}, "resolution": {{"status":"resolved_exact","type_ids":[{concrete_npc_tid}]}} }}
              ]
            }},
            {{
              "wave_index": 2,
              "timing": "sequential_wave",
              "spawns": [{{ "raw_class": "Cruiser", "raw_name": "R2 Remote Wave", "count": {{"min":1,"max":1}}, "resolution": {{"status":"resolved_exact","type_ids":[{concrete_npc_tid}]}} }}]
            }}
          ]
        }},
        {{
          "room_index": 3,
          "waves": [{{ "wave_index": 1, "timing": "initial_spawn", "spawns": [{{ "raw_class":"Battleship","count":{{"min":1,"max":1}},"resolution":{{"status":"resolved_exact","type_ids":[{concrete_npc_tid}]}} }}] }}]
        }}
      ]
    }},
    {{
      "entry_id": "cross_room_combat_fallback",
      "base_title": "Cross Room Combat Fallback",
      "sde_matches": {{"dungeon_ids":[{did_combat_cross}],"mission_ids":[]}},
      "rooms": [
        {{
          "room_index": 1,
          "waves": [{{
            "wave_index": 1,
            "timing": "initial_spawn",
            "wave_trigger": {{ "kind": "npc_damaged", "hull_below_pct": 50, "target_room_index": 2, "unlocks_gate": true }},
            "spawns": [{{ "raw_class": "Cruiser", "count": {{"min":1,"max":1}}, "resolution": {{"status":"resolved_exact","type_ids":[{concrete_npc_tid}]}} }}]
          }}]
        }},
        {{ "room_index": 2, "waves": [{{ "wave_index": 1, "timing": "initial_spawn", "spawns": [{{ "raw_class":"Frigate","count":{{"min":1,"max":1}},"resolution":{{"status":"resolved_exact","type_ids":[{concrete_npc_tid}]}} }}] }}] }}
      ]
    }},
    {{
      "entry_id": "noncontig_valid",
      "base_title": "Noncontiguous Rooms",
      "sde_matches": {{"dungeon_ids":[{did_noncontig_ok}],"mission_ids":[]}},
      "rooms": [
        {{ "room_index": 2, "waves": [{{ "wave_index": 1, "timing": "initial_spawn", "spawns": [{{ "raw_class":"Frigate","count":{{"min":1,"max":1}},"resolution":{{"status":"resolved_exact","type_ids":[{concrete_npc_tid}]}} }}] }}] }},
        {{ "room_index": 5, "waves": [{{ "wave_index": 1, "timing": "sequential_wave", "wave_trigger": {{ "kind":"room_entered","target_room_index":5,"action":{{"kind":"trigger_message","message_type":1,"message_id":99}} }}, "spawns": [{{ "raw_class":"Cruiser","count":{{"min":1,"max":1}},"resolution":{{"status":"resolved_exact","type_ids":[{concrete_npc_tid}]}} }}] }}] }}
      ]
    }},
    {{
      "entry_id": "unclassified_obj_entry",
      "base_title": "Unclassified Objective Site",
      "objective": {{ "classification_state": "Unclassified", "kind": null, "original_text": "Hack relay node." }},
      "sde_matches": {{"dungeon_ids":[{did_uncl_obj}],"mission_ids":[]}},
      "rooms": [{{ "room_index": 1, "waves": [{{ "wave_index": 1, "timing": "initial_spawn", "spawns": [{{ "raw_class":"Frigate","count":{{"min":1,"max":1}},"resolution":{{"status":"resolved_exact","type_ids":[{concrete_npc_tid}]}} }}] }}] }}]
    }}
  ]
}}"#
    );
    let cat = Arc::new(parse_and_validate_wiki_pve_json(&combined_json).expect("valid F-01 / F-02 combined JSON"));
    install_wiki_pve_catalog_for_test(&ctx, cat);

    // Build checks: cross-room combat Room 1 gets fallback OnKill(None)->UnlockGate; non-contiguous Room 5 maps to Some(1); F-02 preserves "unclassified".
    let combat_cross = build_wiki_deadspace_rooms(&ctx, sys_id, DungeonTemplateId::new(did_combat_cross), "Cross Room Combat Fallback", [0.0; 3], false).expect("combat cross build");
    assert!(combat_cross.rooms[0].triggers.iter().any(|t| matches!((&t.kind, &t.action), (DungeonTriggerKind::OnKill { target_npc_group: None, .. }, DungeonTriggerAction::UnlockGate)) && t.target_room_index.is_none()));
    assert!(combat_cross.rooms[0].triggers.iter().any(|t| t.target_room_index == Some(1) && t.all_actions().any(|a| *a == DungeonTriggerAction::CompleteDungeon)));

    let noncontig = build_wiki_deadspace_rooms(&ctx, sys_id, DungeonTemplateId::new(did_noncontig_ok), "Noncontiguous Rooms", [0.0; 3], false).expect("noncontig build");
    assert!(noncontig.rooms[1].triggers.iter().any(|t| matches!(t.kind, DungeonTriggerKind::OnRoomEntered { target_room_index: Some(1) })));

    let uncl_obj_build = build_wiki_deadspace_rooms(&ctx, sys_id, DungeonTemplateId::new(did_uncl_obj), "Unclassified Objective Site", [0.0; 3], false).expect("unclassified obj build");
    assert_eq!(uncl_obj_build.objectives.len(), 1);
    assert_eq!(uncl_obj_build.objectives[0].kind, "unclassified");
    assert_ne!(uncl_obj_build.objectives[0].kind, "kill_all");

    // Live runtime check on 3-room cross-room complex: Room 1 0-NPC gate NOT locked; Room 1 c1 not spawn_on_trigger;
    // Room 2 c2 IS spawn_on_trigger, Room 2 gate starts locked, and looting Room 1 triggers Room 2 object + Wave 2 + gate unlock.
    let char_cr = 90_001_602_i64;
    let inst_cr = spawn_sde_dungeon_runtime_instance(&ctx, char_cr, sys_id, did_cross, "Deadspace", [105_000_000.0, 0.0, 0.0], false).await.expect("spawn cross-room dungeon");
    let r1_cont = read_dungeon_runtime_store(&ctx, |store| {
        let rt = &store.instances[&inst_cr];
        assert!(!rt.rooms[0].gate_locked, "0-NPC Room 1 whose only UnlockGate targets Room 2 must not deadlock");
        assert!(!rt.rooms[0].objects[0].spawn_on_trigger && rt.rooms[0].objects[0].spawned);
        assert!(rt.rooms[1].gate_locked && rt.rooms[1].gate_links[0].locked, "Room 2 targeted by cross-room UnlockGate must start locked");
        assert!(rt.rooms[1].objects[0].spawn_on_trigger && !rt.rooms[1].objects[0].spawned);
        assert!(rt.rooms[1].triggers.iter().all(|t| !matches!(&t.kind, DungeonTriggerKind::OnKill { target_npc_group: Some(1), .. } if t.action == DungeonTriggerAction::SpawnNpcGroup(2))));
        rt.rooms[0].objects[0].ball_id
    });
    on_dungeon_item_looted(&ctx, char_cr, Some(sys_id), r1_cont, 34).await;
    read_dungeon_runtime_store(&ctx, |store| {
        let rt = &store.instances[&inst_cr];
        assert!(!rt.rooms[1].gate_locked && !rt.rooms[1].gate_links[0].locked, "looting Room 1 unlocks Room 2 gate");
        assert!(rt.rooms[1].objects[0].spawned, "Room 2 R2 Hidden Cache spawns on Room 1 loot trigger");
        assert!(rt.rooms[1].npc_groups[1].spawned, "Room 2 Wave 2 spawns on Room 1 cross-room target_wave_index");
    });

    // F-02 committed fixture check against Synthetic Raid#6 (Angel Cartel, dungeon_id 4003).
    let raw_syn = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/synthetic_wiki_pve_content.json")).expect("read fixture");
    let cat_syn = Arc::new(parse_and_validate_wiki_pve_json(&raw_syn).expect("valid fixture"));
    install_wiki_pve_catalog_for_test(&ctx, cat_syn);
    let syn6 = build_wiki_deadspace_rooms(&ctx, 30_000_999, DungeonTemplateId::new(4003), "First Location", [0.0; 3], false).expect("Synthetic Raid#6 build");
    assert_eq!(syn6.objectives.len(), 1);
    assert_eq!(syn6.objectives[0].kind, "unclassified");
    assert_ne!(syn6.objectives[0].kind, "kill_all");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r6_bf01_on_npc_damaged_tags_and_bf02_on_approach_object_binding() {
    let sde = shared_sde();
    let ctx = make_ctx(Arc::clone(&sde));
    let concrete_npc_tid = pick_concrete_combat_npc_tid(&sde);
    let sys_id = 30_000_142_u32;
    let (did_backref, did_skipped, did_spawn_obj, did_second_cont) = (991_701_u32, 991_702_u32, 991_703_u32, 991_704_u32);

    let json = format!(
        r#"{{
  "header":{{"schema_version":2,"source":"EVE University Wiki (CC BY-SA 4.0)","license":"CC BY-SA 4.0"}},
  "entries":[
    {{
      "entry_id":"bf01_backref_on_attack",
      "base_title":"Backref OnAttack Complex",
      "sde_matches":{{"dungeon_ids":[{did_backref}],"mission_ids":[]}},
      "rooms":[{{
        "room_index":1,
        "waves":[
          {{
            "wave_index":1,
            "timing":"initial_spawn",
            "spawns":[
              {{"raw_class":"Frigate","raw_name":"Trigger Frigate","count":{{"min":1,"max":1}},"resolution":{{"status":"resolved_exact","type_ids":[{concrete_npc_tid}]}}}},
              {{"raw_class":"Frigate","raw_name":"Untagged Escort","count":{{"min":1,"max":1}},"resolution":{{"status":"resolved_exact","type_ids":[{concrete_npc_tid}]}}}}
            ]
          }},
          {{
            "wave_index":2,
            "timing":"sequential_wave",
            "wave_trigger":{{"kind":"npc_damaged","condition":"on_attack","source_wave_index":1,"source_spawn_index":1,"trigger_ship_tag":"r1_w1_s1"}},
            "spawns":[{{"raw_class":"Cruiser","count":{{"min":1,"max":1}},"resolution":{{"status":"resolved_exact","type_ids":[{concrete_npc_tid}]}}}}]
          }}
        ]
      }}]
    }},
    {{
      "entry_id":"bf01_skipped_source_on_attack",
      "base_title":"Skipped Source OnAttack",
      "sde_matches":{{"dungeon_ids":[{did_skipped}],"mission_ids":[]}},
      "rooms":[{{
        "room_index":1,
        "waves":[
          {{
            "wave_index":1,
            "timing":"initial_spawn",
            "spawns":[
              {{"raw_class":"Frigate","raw_name":"Ghost Ship","tag":"r1_w1_ghost","count":{{"min":0,"max":0}},"resolution":{{"status":"resolved_exact","type_ids":[{concrete_npc_tid}]}}}},
              {{"raw_class":"Frigate","raw_name":"Live Guard","count":{{"min":1,"max":1}},"resolution":{{"status":"resolved_exact","type_ids":[{concrete_npc_tid}]}}}}
            ]
          }},
          {{
            "wave_index":2,
            "timing":"sequential_wave",
            "wave_trigger":{{"kind":"npc_damaged","condition":"on_attack","source_wave_index":1,"trigger_ship_tag":"r1_w1_ghost"}},
            "spawns":[{{"raw_class":"Cruiser","count":{{"min":1,"max":1}},"resolution":{{"status":"resolved_exact","type_ids":[{concrete_npc_tid}]}}}}]
          }}
        ]
      }}]
    }},
    {{
      "entry_id":"bf02_spawn_on_trigger_on_approach",
      "base_title":"SpawnObject Plus OnApproach",
      "sde_matches":{{"dungeon_ids":[{did_spawn_obj}],"mission_ids":[]}},
      "rooms":[{{
        "room_index":1,
        "waves":[
          {{
            "wave_index":1,
            "timing":"initial_spawn",
            "spawns":[
              {{"raw_class":"Container","raw_name":"Reward Cache","count":{{"min":1,"max":1}},"resolution":{{"status":"resolved","type_ids":[23]}}}},
              {{"raw_class":"Frigate","count":{{"min":1,"max":1}},"resolution":{{"status":"resolved_exact","type_ids":[{concrete_npc_tid}]}},"trigger":{{"kind":"this_ship_destroyed","action":{{"kind":"spawn_object","object_index":0}}}}}}
            ]
          }},
          {{
            "wave_index":2,
            "timing":"sequential_wave",
            "wave_trigger":{{"kind":"ship_approached","radius_m":4000}},
            "spawns":[{{"raw_class":"Cruiser","count":{{"min":1,"max":1}},"resolution":{{"status":"resolved_exact","type_ids":[{concrete_npc_tid}]}}}}]
          }}
        ]
      }}]
    }},
    {{
      "entry_id":"bf02_second_container_approach_and_loot",
      "base_title":"Second Container Approach",
      "sde_matches":{{"dungeon_ids":[{did_second_cont}],"mission_ids":[]}},
      "rooms":[{{
        "room_index":1,
        "waves":[
          {{
            "wave_index":1,
            "timing":"initial_spawn",
            "spawns":[
              {{"raw_class":"Container","raw_name":"First Delayed Cache","count":{{"min":1,"max":1}},"resolution":{{"status":"resolved","type_ids":[23]}}}},
              {{"raw_class":"Container","raw_name":"Second Approach Cache","count":{{"min":1,"max":1}},"resolution":{{"status":"resolved","type_ids":[23]}},"trigger":{{"kind":"ship_approached","radius_m":3500,"action":{{"kind":"spawn_object","object_index":0}}}}}}
            ]
          }},
          {{
            "wave_index":2,
            "timing":"sequential_wave",
            "wave_trigger":{{"kind":"loot_taken","item_type_id":34}},
            "spawns":[{{"raw_class":"Frigate","count":{{"min":1,"max":1}},"resolution":{{"status":"resolved_exact","type_ids":[{concrete_npc_tid}]}}}}]
          }}
        ]
      }}]
    }}
  ]
}}"#
    );
    let cat = Arc::new(parse_and_validate_wiki_pve_json(&json).expect("valid R6 JSON"));
    install_wiki_pve_catalog_for_test(&ctx, cat);

    // Prepare online pilot + ship in sys_id for live damage & approach verification.
    let char_id = 90_001_701_i64;
    ctx.db.provision_gm_account("r6_runner", "pw", char_id, "R6Runner");
    let (uid, ship_id) = ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.solar_system_id = sys_id;
        ch.station_id = None;
        ch.structure_id = None;
        (ch.user_id, ch.active_ship_id)
    });
    ctx.db.acquire_online_lock_with_computer_hash(uid, char_id as u64, Some(char_id), None, true, 1_776_400_000).expect("online lock");
    let sys_h = ctx.systems.get_or_spawn(sys_id);

    // BF-01(a): back-referenced source_spawn_index=1 assigns r1_w1_s1 to spawns[0], attacking spawns[0] spawns Wave 2 and clears cleanly.
    let inst_br = spawn_sde_dungeon_runtime_instance(&ctx, char_id, sys_id, did_backref, "Deadspace", [110_000_000.0, 0.0, 0.0], false).await.expect("spawn backref");
    let (trig_npc, other_npc) = read_dungeon_runtime_store(&ctx, |s| {
        let r0 = &s.instances[&inst_br].rooms[0];
        assert!(r0.triggers.iter().any(|t| matches!(&t.kind, DungeonTriggerKind::OnNpcDamaged { target_tag: Some(tag), .. } if tag == "r1_w1_s1")));
        (r0.npc_groups[0].npc_ball_ids[0], r0.npc_groups[0].npc_ball_ids[1])
    });
    let non_lethal_tap = |bid: i64| {
        let d = sys_h
            .view()
            .combat_states
            .get(&bid)
            .map(|cs| ((cs.max_shield_hp + cs.max_armor_hp + cs.max_structure_hp) * 0.05).max(25.0))
            .unwrap_or(25.0);
        eve_destiny::DamageVector { em: d, thermal: d, kinetic: d, explosive: d }
    };
    let lethal = eve_destiny::DamageVector { em: 500_000.0, thermal: 500_000.0, kinetic: 500_000.0, explosive: 500_000.0 };
    let _ = sys_h.apply_damage_with_attacker(other_npc, non_lethal_tap(other_npc), Some(char_id)).await.unwrap();
    sys_h.step_manual(1.0).await.unwrap();
    read_dungeon_runtime_store(&ctx, |s| assert!(!s.instances[&inst_br].rooms[0].npc_groups[1].spawned));
    let _ = sys_h.apply_damage_with_attacker(trig_npc, non_lethal_tap(trig_npc), Some(char_id)).await.unwrap();
    sys_h.step_manual(1.0).await.unwrap();
    read_dungeon_runtime_store(&ctx, |s| assert!(s.instances[&inst_br].rooms[0].npc_groups[1].spawned, "attacking back-referenced ship spawns Wave 2"));
    let _ = sys_h.apply_damage_with_attacker(trig_npc, lethal, Some(char_id)).await.unwrap();
    let _ = sys_h.apply_damage_with_attacker(other_npc, lethal, Some(char_id)).await.unwrap();
    let w2_br = read_dungeon_runtime_store(&ctx, |s| s.instances[&inst_br].rooms[0].npc_groups[1].npc_ball_ids[0]);
    let _ = sys_h.apply_damage_with_attacker(w2_br, lethal, Some(char_id)).await.unwrap();
    read_dungeon_runtime_store(&ctx, |s| assert!(s.instances[&inst_br].completed));

    // BF-01(b): skipped 0-count source spawn clears OnNpcDamaged.target_tag -> None so attacking live Wave 1 guard spawns Wave 2.
    let inst_sk = spawn_sde_dungeon_runtime_instance(&ctx, char_id, sys_id, did_skipped, "Deadspace", [112_000_000.0, 0.0, 0.0], false).await.expect("spawn skipped");
    let sk_npc = read_dungeon_runtime_store(&ctx, |s| {
        let r0 = &s.instances[&inst_sk].rooms[0];
        assert!(r0.triggers.iter().any(|t| matches!(t.kind, DungeonTriggerKind::OnNpcDamaged { target_npc_group: Some(1), target_tag: None, .. })));
        r0.npc_groups[0].npc_ball_ids[0]
    });
    let _ = sys_h.apply_damage_with_attacker(sk_npc, non_lethal_tap(sk_npc), Some(char_id)).await.unwrap();
    sys_h.step_manual(1.0).await.unwrap();
    read_dungeon_runtime_store(&ctx, |s| assert!(s.instances[&inst_sk].rooms[0].npc_groups[1].spawned, "fallback group OnNpcDamaged spawns Wave 2"));

    // BF-02(a): OnApproach does not bind to spawn_on_trigger objects[0]; live approach spawns Wave 2.
    let inst_ap = spawn_sde_dungeon_runtime_instance(&ctx, char_id, sys_id, did_spawn_obj, "Deadspace", [115_000_000.0, 0.0, 0.0], false).await.expect("spawn approach");
    let room_pos = read_dungeon_runtime_store(&ctx, |s| {
        let r0 = &s.instances[&inst_ap].rooms[0];
        assert!(r0.objects[0].spawn_on_trigger && !r0.objects[0].spawned);
        assert!(r0.triggers.iter().any(|t| matches!(t.kind, DungeonTriggerKind::OnApproach { target_object_index: None, anchor_pos, .. } if anchor_pos == r0.position) && t.action == DungeonTriggerAction::SpawnNpcGroup(2)));
        r0.position
    });
    sys_h.spawn_ball(eve_destiny::Ball { id: ship_id, type_id: 587, position: glam::DVec3::from_array(room_pos), velocity: glam::DVec3::ZERO, max_velocity: 300.0, inertia: 1.0, mass: 1_000_000.0, radius: 40.0, speed_fraction: 0.0, mode: eve_destiny::MovementMode::Stop }).await.unwrap();
    let _ = crate::services::pve_dungeon_runtime_rpc::check_dungeon_triggers(&ctx, char_id, sys_id, ship_id).await;
    read_dungeon_runtime_store(&ctx, |s| assert!(s.instances[&inst_ap].rooms[0].npc_groups[1].spawned, "OnApproach fires without deadlocking on spawn_on_trigger objects[0]"));

    // BF-02(b): Second container spawn omitted ship_approached binds to Some(1) + objects[1].position, and auto OnLoot rebinds off spawn_on_trigger objects[0] to Some(1).
    let b2 = build_wiki_deadspace_rooms(&ctx, sys_id, DungeonTemplateId::new(did_second_cont), "Second Container Approach", [120_000_000.0, 0.0, 0.0], false).expect("second cont build");
    let r0_b2 = &b2.rooms[0];
    assert!(r0_b2.objects[0].spawn_on_trigger && !r0_b2.objects[1].spawn_on_trigger);
    assert!(r0_b2.triggers.iter().any(|t| matches!(t.kind, DungeonTriggerKind::OnApproach { target_object_index: Some(1), anchor_pos, .. } if anchor_pos == r0_b2.objects[1].position)));
    assert!(r0_b2.triggers.iter().any(|t| matches!(t.kind, DungeonTriggerKind::OnLoot { target_object_index: Some(1), .. })));
}
