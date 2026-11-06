//! `services` — Modular 12-Domain RPC & Quasar Service Handlers (`M1`–`M8`).
//!
//! Organizes all 216 cataloged `machoNet` RPC services (`193` V24.01 active + `23` legacy-only)
//! and all 135 Quasar Protobuf domains (`69` `eve_public.*` + `66` `eve.*` internal) into 12 cohesive
//! domain modules matching `PLAN.md` §3.12 and `docs/spec/01..05`.

#![forbid(unsafe_code)]

pub mod account_services;
pub mod air_npe_nodegraph_pocket;
pub mod air_npe_nodegraph_runner;
pub mod abyssal_void_gates;
pub mod abyssal_void_mgr;
pub(crate) mod agents_mission_delivery;
pub mod agents_mission_rewards;
pub mod agents_missions;
pub mod auth_cache;
pub mod campaigns_and_milestones;
pub mod char_selection;
pub mod character_energy;
pub mod character_lifecycle;
pub mod character_skills;
pub use character_skills::character_contacts;
pub mod character_expert_systems;
pub mod character_clone_jump_rules;
pub mod character_skill_ops;
pub mod character_certificates;
pub mod client_login;
pub mod cluster_macho_misc_services;
pub mod combat_crimewatch;
pub mod combat_npc_encounters;
pub mod combat_npc_police;
pub use combat_crimewatch::combat_weapons;
pub use combat_crimewatch::combat_weapons_spool;
pub mod config_service;
pub mod contracts_service;
pub use contracts_service as contracts;
pub mod corp_medals_bulletins;
pub mod corp_registry_ops;
pub mod cosmetics_events;
pub mod cosmetics_ui_ops;
pub mod encounter_surveillance_system;
pub mod fast_checkout_and_activity;
pub mod fitting_saved_apply;
pub mod fitting_cycles_overheat;
pub mod fleet_social;
pub mod fw_enlistment;
pub mod fw_gameplay;
pub mod fw_services;
pub mod hypernet_raffle_ops;
pub mod implant_plugging;
pub mod ingamereport_ops;
pub mod industry_pi;
pub mod insurgency_complex_ops;
pub mod insurgency_services;
pub mod inventory_bound;
pub mod inventory_fitting;
pub mod inventory_stacking;
pub mod jump_clone_names;
pub mod lp_store;
pub mod lp_store_catalog;
pub mod lookup_svc;
pub mod macho_cluster_mgmt;
pub mod mail_notify;
pub mod map_service;
pub mod mining_phased_autotarget;
pub mod market_contracts;
pub mod market_price_history;
pub mod market_trading;
pub mod mining_advanced;
pub mod mining_cycle;
pub mod mining_sites;
pub mod mobile_deployables;
pub mod mobile_deployables_ext;
pub mod nav_warp_interdiction;
pub mod notify_emitters_character_services;
pub mod notify_emitters_space_dungeons;
pub mod notify_emitters_combat_ess_progression;
pub mod notify_emitters_seasons_tactical;
pub mod notify_emitters_operations_nodegraph;
pub mod notify_emitters_skills_trade_godma;
pub mod crate_loot_service;
pub mod notify_emitters_tournaments_anomalies;
pub mod notify_emitters_channel_events;
pub mod notify_emitters_fleet_combat;
pub mod notify_emitters_ess_bounties;
pub mod notify_emitters_missing;
pub mod notify_emitters_missing2;
pub mod operations_manager;
pub mod operations_manager_helpers;
pub mod operations_survey_handlers;
pub mod petitioner_ops;
pub mod project_discovery;
pub mod pve_dungeon_cache;
pub mod pve_dungeon_editor;
pub mod pve_dungeon_runtime;
pub mod pve_dungeon_runtime_rpc;
pub mod pve_expeditions;
pub mod pve_exploration;
pub mod pve_exploration_seeding;
pub mod pve_exploration_sites;
pub mod pve_hacking_board;
pub mod pve_hacking_sites;
pub mod pve_homefront_sites;
pub mod pve_incursion_lifecycle;
pub mod pve_incursion_sites;
pub mod pve_metaliminal_storms;
pub mod pve_ess_reserve_banks;
pub mod pve_sleeper_caches;
pub mod pve_abyssal_hazards;
pub mod pve_abyssal_lifecycle;
pub mod pve_abyssal_pylons;
pub mod pve_crab_beacons;
pub mod pve_special_sites;
pub mod pve_special_sites_abyssal;
pub mod pve_special_sites_combat;
pub(crate) mod r6_t1_p0_helpers;
pub mod pvp_filament_arena;
pub mod pvp_filament_mgr;
pub mod quasar_achievements;
pub mod quasar_achievements_ops;
pub mod quasar_assetholding_entitlements;
pub mod quasar_entitlement_ledger;
pub mod quasar_auth_user;
pub mod quasar_market_contract_services;
pub mod quasar_corp_structure_services;
pub mod quasar_debug_race_ship_services;
pub mod quasar_campaign_definitions;
pub mod quasar_campaign_handlers;
pub mod quasar_campaign_objective_handlers;
pub mod quasar_campaign_state;
pub mod quasar_career_goals;
pub mod quasar_career_goals_wire;
pub mod quasar_cosmetics_live;
pub mod quasar_cosmetics_market;
pub mod quasar_cosmetics_market_types;
pub mod quasar_cosmetics_services;
pub mod quasar_cosmetics_drafts;
pub mod quasar_cosmetics_skinr;
pub mod quasar_cosmetics_skinr_seq;
pub mod quasar_daily_goals;
pub mod quasar_economy_planetary_industry;
pub mod quasar_entitlement_membership;
pub mod quasar_inventory_asset;
pub mod quasar_market;
pub mod quasar_payment_store;
pub mod quasar_payment_store_types;
pub mod quasar_progression;
pub mod quasar_goal_tale_notices;
pub mod quasar_progression_extra;
pub mod quasar_skills_clones;
pub mod quasar_social;
pub mod quasar_social_chat;
pub mod quasar_social_community;
pub mod quasar_social_corp;
pub mod quasar_corp_domain;
pub mod quasar_freelance_defs;
pub mod quasar_social_freelance;
pub mod quasar_sov_services;
pub mod quasar_sov_hub;
pub mod quasar_sov_mercden;
pub mod quasar_sov_resources;
pub mod quasar_sov_skyhook;
pub mod quasar_sov_workforce;
pub mod quasar_space_domain;
pub mod doomsday_aoe_ops;
pub mod doomsday_aoe_tick;
pub mod ice_heist_ops;
pub mod pve_fob_observatory;
pub mod quasar_space_services;
pub mod quasar_char_skill_services;
pub mod quasar_fleet_navigation_services;
pub mod season_fsd;
pub mod season_manager;
pub mod security_mgr;
pub mod serverops_cluster;
pub mod shipcaster_ops;
pub mod social_corp_fleet;
pub mod sov_fw_insurgency;
pub mod sov_gameplay;
pub mod sov_services;
pub mod sov_warzone_dynamic;
pub mod space_destiny;
pub use space_destiny::space_warp_jump;
pub mod space_entity_mining;
pub mod space_entity_ops;
pub mod space_mining_harvest;
pub mod space_navigation_advanced;
pub mod station_structure;
pub mod station_structure_handlers;
pub mod structure_acl_defense_rules;
pub mod structure_anchoring_rules;
pub mod structure_control_ops;
pub mod structure_profile_services;
pub mod structure_standup_weapons;
pub mod structure_weapons_ops;
pub mod tactical_module_mechanics;
pub mod tactical_modules;
pub mod tactical_modules_probes;
pub mod tourney_service;
pub mod wallet_ledger;
pub mod wallet_serverops;
pub mod wallet_services_ext;
pub mod wormhole_lifecycle_ops;
pub mod wormhole_pve_escalations;
pub mod wormhole_pve_seeding;
pub mod quasar_vendors_and_war_services;

pub use inventory_fitting::fitting_containers_ships;
pub use space_destiny::space_probe_scanning;

#[cfg(test)]
mod combat_crimewatch_extra_unit_tests;
#[cfg(test)]
mod combat_fighters_extra_unit_tests;
#[cfg(test)]
mod combat_weapons_extra_unit_tests;
#[cfg(test)]
mod combat_weapons_spool_extra_unit_tests;
#[cfg(test)]
mod contracts_service_extra_unit_tests;
#[cfg(test)]
mod fitting_containers_ships_extra_unit_tests;
#[cfg(test)]
mod fitting_containers_ships_unit_tests;
#[cfg(test)]
mod fitting_module_cycles_helpers_extra_unit_tests;
#[cfg(test)]
mod fleet_social_corp_gov_extra_unit_tests;
#[cfg(test)]
mod fw_gameplay_insurgency_extra_unit_tests;
#[cfg(test)]
mod inventory_stacking_extra_unit_tests;
#[cfg(test)]
mod pve_dungeon_runtime_extra_unit_tests;
#[cfg(test)]
mod pve_ess_reserve_banks_extra_unit_tests;
#[cfg(test)]
mod fitting_ship_bays_extra_unit_tests;
#[cfg(test)]
mod insurgency_complex_ops_extra_unit_tests;
#[cfg(test)]
mod pve_exploration_extra_unit_tests;
#[cfg(test)]
mod space_destiny_extra_unit_tests;
#[cfg(test)]
mod social_corp_fleet_extra_unit_tests;
#[cfg(test)]
mod r7_tb_p0_security_tests;
#[cfg(test)]
mod r7_td_p1_security_tests;
#[cfg(test)]
mod r7_td_p1_security_tests_part2;
#[cfg(test)]
mod space_probe_scanning_extra_unit_tests;
#[cfg(test)]
mod space_probe_scanning_unit_tests;
#[cfg(test)]
mod fitting_scanners_superweapons_extra_unit_tests;
#[cfg(test)]
mod macho_cluster_mgmt_extra_unit_tests;
#[cfg(test)]
mod tactical_modules_extra_unit_tests;
#[cfg(test)]
mod quasar_cosmetics_market_extra_unit_tests;
#[cfg(test)]
mod quasar_cosmetics_skinr_extra_unit_tests;
#[cfg(test)]
mod quasar_skills_clones_extra_unit_tests;
#[cfg(test)]
mod fw_gameplay_extra_unit_tests;
#[cfg(test)]
mod structure_lifecycle_rpc_extra_unit_tests;
#[cfg(test)]
mod auth_cache_extra_unit_tests;
#[cfg(test)]
mod sov_gameplay_entosis_extra_unit_tests;
#[cfg(test)]
mod fitting_module_cycles_extra_unit_tests;
#[cfg(test)]
mod combat_npc_ai_extra_unit_tests;
#[cfg(test)]
mod station_structure_extra_unit_tests;
#[cfg(test)]
mod structure_lifecycle_extra_unit_tests;
#[cfg(test)]
mod pve_incursion_lifecycle_extra_unit_tests;
#[cfg(test)]
mod pi_helpers_extra_unit_tests;
#[cfg(test)]
mod combat_fighters_abilities_extra_unit_tests;
#[cfg(test)]
mod wormhole_lifecycle_ops_extra_unit_tests;
#[cfg(test)]
mod fleet_social_corp_tail_extra_unit_tests;
#[cfg(test)]
mod season_manager_extra_unit_tests;
#[cfg(test)]
mod quasar_progression_extra_extra_unit_tests;
#[cfg(test)]
mod mining_sites_extra_unit_tests;
#[cfg(test)]
mod tourney_service_extra_unit_tests;
#[cfg(test)]
mod quasar_space_services_extra_unit_tests;
#[cfg(test)]
mod project_discovery_extra_unit_tests;
#[cfg(test)]
mod combat_legacy_ess_extra_unit_tests;
#[cfg(test)]
mod serverops_cluster_extra_unit_tests;
#[cfg(test)]
mod agents_missions_helpers_extra_unit_tests;
#[cfg(test)]
mod notify_emitters_skills_trade_godma_extra_unit_tests;
#[cfg(test)]
mod mailing_lists_ops_extra_unit_tests;
#[cfg(test)]
mod implant_plugging_extra_unit_tests;
#[cfg(test)]
mod tactical_module_mechanics_extra_unit_tests;
#[cfg(test)]
mod contracts_deep_extra_unit_tests;
#[cfg(test)]
mod industry_deep_extra_unit_tests;
#[cfg(test)]
mod reprocessing_deep_extra_unit_tests;




#[cfg(test)]
mod fleet_social_finder_tactical_extra_unit_tests;

#[cfg(test)]
mod quasar_auth_user_extra_unit_tests;

#[cfg(test)]
mod jump_clone_names_extra_unit_tests;

#[cfg(test)]
mod fitting_dogma_im_extra_unit_tests;

#[cfg(test)]
mod pve_abyssal_pylons_extra_unit_tests;

#[cfg(test)]
mod pvp_filament_arena_extra_unit_tests;

#[cfg(test)]
mod character_gm_tools_extra_unit_tests;

#[cfg(test)]
mod character_skills_extra_unit_tests;

#[cfg(test)]
mod mobile_deployables_extra_unit_tests;

#[cfg(test)]
mod space_nav_travel_extra_unit_tests;

#[cfg(test)]
mod wallet_services_ext_extra_unit_tests;

#[cfg(test)]
mod structure_upwell_ops_extra_unit_tests;

#[cfg(test)]
mod crimewatch_bounty_extra_unit_tests;

#[cfg(test)]
mod sov_services_extra_unit_tests;

#[cfg(test)]
mod notify_emitters_tournaments_anomalies_extra_unit_tests;

#[cfg(test)]
mod petitioner_ops_extra_unit_tests;

#[cfg(test)]
mod notify_emitters_character_services_extra_unit_tests;

#[cfg(test)]
mod quasar_corp_structure_services_extra_unit_tests;

#[cfg(test)]
mod cluster_macho_misc_services_extra_unit_tests;

#[cfg(test)]
mod fleet_social_calendar_ops_extra_unit_tests;

#[cfg(test)]
mod operations_manager_helpers_extra_unit_tests;

#[cfg(test)]
mod pve_fob_observatory_extra_unit_tests;

#[cfg(test)]
mod pve_hacking_sites_extra_unit_tests;

#[cfg(test)]
mod wormhole_pve_escalations_extra_unit_tests;

#[cfg(test)]
mod fitting_saved_apply_extra_unit_tests;

#[cfg(test)]
mod doomsday_aoe_ops_extra_unit_tests;

#[cfg(test)]
mod pve_special_sites_abyssal_extra_unit_tests;

#[cfg(test)]
mod pve_expeditions_extra_unit_tests;

#[cfg(test)]
mod combat_npc_encounters_extra_unit_tests;

#[cfg(test)]
mod fw_services_extra_unit_tests;

#[cfg(test)]
mod character_lifecycle_extra_unit_tests;

#[cfg(test)]
mod pve_dungeon_editor_extra_unit_tests;

#[cfg(test)]
mod fleet_social_extra_unit_tests;

#[cfg(test)]
mod nav_warp_interdiction_extra_unit_tests;

#[cfg(test)]
mod fleet_social_persistence_extra_unit_tests;

#[cfg(test)]
mod quasar_vendors_and_war_services_extra_unit_tests;

#[cfg(test)]
mod quasar_market_extra_unit_tests;

#[cfg(test)]
mod fitting_mining_lasers_extra_unit_tests;

#[cfg(test)]
mod lookup_svc_extra_unit_tests;

#[cfg(test)]
mod market_price_history_extra_unit_tests;

#[cfg(test)]
mod fleet_social_corp_tail_dispatch_extra_unit_tests;

#[cfg(test)]
mod quasar_sov_mercden_extra_unit_tests;

#[cfg(test)]
mod corp_registry_roles_deep_extra_unit_tests;

#[cfg(test)]
mod sov_fw_deep_extra_unit_tests;

#[cfg(test)]
mod fleet_social_deep_extra_unit_tests;
#[cfg(test)]
mod combat_crimewatch_deep_extra_unit_tests;
#[cfg(test)]
mod space_destiny_deep_extra_unit_tests;
#[cfg(test)]
mod wormhole_lifecycle_deep_extra_unit_tests;
#[cfg(test)]
mod structures_pos_deep_extra_unit_tests;
#[cfg(test)]
mod planetary_interaction_deep_extra_unit_tests;
#[cfg(test)]
mod abyssal_void_mgr_deep_extra_unit_tests;
#[cfg(test)]
mod pve_special_sites_abyssal_deep_extra_unit_tests;
#[cfg(test)]
mod pve_incursion_lifecycle_deep_extra_unit_tests;
#[cfg(test)]
mod market_orders_deep_extra_unit_tests;
#[cfg(test)]
mod market_quote_deep_extra_unit_tests;
#[cfg(test)]
mod fitting_containers_ships_deep_extra_unit_tests;
#[cfg(test)]
mod space_probe_scanning_deep_extra_unit_tests;
#[cfg(test)]
mod character_skill_clone_deep_extra_unit_tests;
#[cfg(test)]
mod mining_sites_deep_extra_unit_tests;
#[cfg(test)]
mod mining_cycle_deep_extra_unit_tests;
#[cfg(test)]
mod moon_mining_deep_extra_unit_tests;
#[cfg(test)]
mod pve_missions_deep_extra_unit_tests;
#[cfg(test)]
mod pve_burners_lp_escalations_deep_extra_unit_tests;
#[cfg(test)]
mod wallet_accounting_store_deep_extra_unit_tests;
#[cfg(test)]
mod mail_chat_notifications_deep_extra_unit_tests;
#[cfg(test)]
mod auth_login_cluster_deep_extra_unit_tests;
#[cfg(test)]
mod station_inventory_stacking_deep_extra_unit_tests;
#[cfg(test)]
mod config_standings_bookmarks_deep_extra_unit_tests;
#[cfg(test)]
mod corp_gov_votes_sanctions_deep_extra_unit_tests;
#[cfg(test)]
mod quasar_goals_achievements_deep_extra_unit_tests;
#[cfg(test)]
mod quasar_cosmetics_skills_deep_extra_unit_tests;
#[cfg(test)]
mod air_npe_season_corp_projects_deep_extra_unit_tests;
#[cfg(test)]
mod crab_beacons_filament_arena_deep_extra_unit_tests;
#[cfg(test)]
mod pve_dungeon_ess_insurgency_deep_extra_unit_tests;
#[cfg(test)]
mod fighters_module_cycles_deep_extra_unit_tests;
#[cfg(test)]
mod structure_lifecycle_control_deep_extra_unit_tests;
#[cfg(test)]
mod mining_compression_cyno_portal_deep_extra_unit_tests;
#[cfg(test)]
mod tactical_scanners_bays_deep_extra_unit_tests;
#[cfg(test)]
mod warp_jump_gm_tools_deep_extra_unit_tests;
#[cfg(test)]
mod r6_t3_p0_security_tests;
#[cfg(test)]
mod r6_t2_p0_security_tests;
#[cfg(test)]
mod r6_t1_p0_security_tests;
pub(crate) mod fleet_social_chat_acl_helpers;
#[cfg(test)]
mod r6_t3_p1_security_tests;
#[cfg(test)]
mod r6_t4_p0_security_tests;
#[cfg(test)]
mod quasar_r6_plex_skin_regression_tests;
#[cfg(test)]
mod r6_t2_p1_security_tests;
#[cfg(test)]
mod r6_t3_p1b_fitting_tests;
#[cfg(test)]
mod r6_t3_p1b_cycles_tests;
pub(crate) mod r6_t1_p1_helpers;
#[cfg(test)]
mod r6_t1_p1_security_tests;
#[cfg(test)]
mod r6_t1_p1_security_tests_part2;
#[cfg(test)]
mod r7_tc_p0_arena_abyssal_tests;
#[cfg(test)]
mod r7_tc_p0_raid_ess_tests;
#[cfg(test)]
mod r6_t4_econ_security_tests;
#[cfg(test)]
mod r7_tb_p1a_security_tests;
#[cfg(test)]
mod r7_tb_p1a_security_tests_part2;
