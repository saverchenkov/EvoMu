//! Unit tests for `pve_dungeon_content` (`schema_version = 2` parser, SDE stand-in resolution,
//! multi-room runtime instantiation, and per-`WorldRepository` catalog isolation for `#1583`).

use super::*;
use crate::router::ServerContext;
use crate::services::pve_dungeon_runtime::{
    materialize_dungeon_room_entities, on_dungeon_npc_killed, read_dungeon_runtime_store,
    resolve_deadspace_or_escalation_warp_target, DungeonTriggerAction, DungeonTriggerKind,
};
use crate::services::pve_dungeon_runtime_rpc::{
    resolve_rooms_with_content_fallback, spawn_sde_dungeon_runtime_instance,
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
    assert!(
        r1.triggers.iter().any(|t| matches!(
            (&t.kind, &t.action),
            (
                DungeonTriggerKind::OnKill {
                    target_npc_group: Some(1),
                    ..
                },
                DungeonTriggerAction::UnlockGate
            )
        )),
        "Wave 1 unlocks_gate spawn trigger must emit UnlockGate"
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
