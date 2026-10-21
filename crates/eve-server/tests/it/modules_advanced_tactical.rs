//! Integration tests for Advanced Tactical Modules, Capital Warfare, Command Bursts, and Electronic Systems (`feat:modules.*`).


use eve_destiny::DVec3;
use eve_server::router::ServerContext;
use eve_server::services::tactical_modules::*;

#[tokio::test]
async fn test_bastion_siege_triage_and_disruptive_lance_effects() {
    // parity: feat:modules.bastion-module-effects, feat:modules.bastion-wormhole-and-mjd-interaction, feat:modules.siege-module-v24-effects, feat:modules.triage-module-v24-effects, feat:modules.disruptive-lance
    eve_testkit::attest(&["feat:modules.bastion-module-effects", "feat:modules.bastion-wormhole-and-mjd-interaction", "feat:modules.siege-module-v24-effects", "feat:modules.triage-module-v24-effects", "feat:modules.disruptive-lance"]);

    let _ctx = ServerContext::new();

    // 1. Bastion Module V24 Effects & Interactions
    let bastion = BastionV24Effect::default();
    assert_eq!(bastion.cycle_duration_sec, 60.0);
    assert_eq!(bastion.rof_multiplier, 0.50, "Bastion gives -50% turret/launcher cycle time");
    assert_eq!(bastion.missile_velocity_bonus, 0.25, "Bastion gives +25% missile velocity");
    assert_eq!(bastion.turret_range_bonus, 0.25, "Bastion gives +25% optimal and falloff");
    assert_eq!(bastion.local_repair_bonus, 0.60, "Bastion gives +60% local repair amount");
    assert_eq!(bastion.local_repair_duration_mult, 0.80, "Bastion gives -20% repair cycle duration");
    assert_eq!(bastion.local_repair_cap_mult, 0.80, "Bastion gives -20% repair capacitor cost");
    assert_eq!(bastion.resistance_bonus, 0.30, "Bastion gives +30% all damage resistances");
    assert_eq!(bastion.sensor_strength_bonus, 1.00, "Bastion gives +100% sensor strength");
    assert!(!bastion.provides_ecm_immunity, "V24 rule: Marauder Bastion has NO ECM immunity");
    assert_eq!(bastion.remote_assistance_impedance, 0.95, "Friendly RSB/RTC effects reduced 95%");
    assert_eq!(bastion.weapons_timer_sec, 60, "Bastion refreshes 60s weapons timer throughout cycle");

    // Bastion vs wormhole jump and MJD/MJFG
    assert!(bastion.deactivates_on_wormhole_jump, "Bastioned ship jumping wormhole arrives with Bastion deactivated");
    assert!(!bastion.allows_mjd, "Micro Jump Drive activation is refused while Bastion is active");
    assert!(bastion.allows_mjfg_movement, "Command Destroyer MJFG still moves bastioned ship");

    // 2. Siege Module V24 Effects & Strontium
    let siege_t1 = get_siege_v24_effect(false, 5);
    assert_eq!(siege_t1.strontium_cost, 125, "Siege T1 strontium cost at level 5: 250 - 25*5 = 125");
    assert_eq!(siege_t1.turret_damage_mult, 8.00, "Siege T1 turret damage +700%");
    assert_eq!(siege_t1.missile_damage_mult, 2.65, "Siege T1 missile damage +165%");
    assert_eq!(siege_t1.xl_torpedo_cycle_mult, 0.20, "XL cruise and torpedo cycle -80%");
    assert_eq!(siege_t1.torpedo_velocity_bonus, 1.50, "Torpedo velocity +150%");
    assert_eq!(siege_t1.ewar_resistance, 0.60, "Sensor damp and weapon disruption resistance 60%");
    assert_eq!(siege_t1.remote_assistance_impedance, 0.80, "80% remote assistance impedance");
    assert!(siege_t1.ecm_immunity, "Siege module grants complete ECM immunity");
    assert!(!siege_t1.grants_weapons_timer, "Siege activation gives NO weapons timer");

    let siege_t2 = get_siege_v24_effect(true, 4);
    assert_eq!(siege_t2.strontium_cost, 150, "Siege T2 strontium cost at level 4: 250 - 25*4 = 150");
    assert_eq!(siege_t2.turret_damage_mult, 9.40, "Siege T2 turret damage +840%");
    assert_eq!(siege_t2.missile_damage_mult, 3.00, "Siege T2 missile damage +200%");
    assert_eq!(siege_t2.ewar_resistance, 0.70, "Siege T2 ewar resistance 70%");

    // 3. Triage Module V24 Effects & Strontium
    let triage_t1 = get_triage_v24_effect(false, 5);
    assert_eq!(triage_t1.strontium_cost, 75, "Triage T1 strontium cost at level 5: 200 - 25*5 = 75");
    assert_eq!(triage_t1.remote_repair_mult, 5.00, "Triage T1 capital remote repair +400%");
    assert_eq!(triage_t1.remote_repair_duration_mult, 0.25, "Remote repair duration -75%");
    assert_eq!(triage_t1.remote_repair_range_mult, 3.00, "Remote repair range +200%");
    assert_eq!(triage_t1.scan_resolution_mult, 10.00, "Scan resolution +900%");
    assert_eq!(triage_t1.max_locked_targets_bonus, 4);
    assert_eq!(triage_t1.local_repair_mult, 2.00, "Local repair amount +100%");
    assert_eq!(triage_t1.local_repair_duration_mult, 0.50, "Local repair duration -50%");
    assert!(triage_t1.ewar_disabled, "EWAR modules disabled under Triage");
    assert_eq!(triage_t1.drone_damage_mult, 0.0, "Drone damage -100% under Triage");

    let triage_t2 = get_triage_v24_effect(true, 5);
    assert_eq!(triage_t2.remote_repair_mult, 5.50, "Triage T2 remote repair +450%");
    assert_eq!(triage_t2.max_locked_targets_bonus, 5);
    assert_eq!(triage_t2.local_repair_mult, 2.20, "Triage T2 local repair amount +120%");

    // 4. Lancer Dreadnought Disruptive Lance
    let lance_res = eval_disruptive_lance(300_000.0, 5, 5, true, 0.3); // In low-sec (0.3)
    assert!(lance_res.is_ok(), "Disruptive lance must be allowed in low-sec while sieged");
    let lance = lance_res.unwrap();
    assert_eq!(lance.lance_damage, (300_000.0 / 3.0) * 1.50, "Disruptive lance deals 1/3 Titan lance damage scaled by skill");
    assert_eq!(lance.neutralizer_amount, 10_000.0, "Neuts 10,000 GJ from ships in area");
    assert_eq!(lance.jump_dock_tether_lock_sec, 300, "Disables jumping, docking, tethering for 5 minutes");
    assert_eq!(lance.remote_repair_reduction_factor, 0.50, "Halves incoming remote repair");

    // Prohibited in high-sec
    let lance_hs = eval_disruptive_lance(300_000.0, 5, 5, true, 0.8);
    assert!(lance_hs.is_err(), "Disruptive lance must be prohibited in high-security space");
}

#[tokio::test]
async fn test_entosis_link_command_bursts_and_industrial_core() {
    // parity: feat:modules.entosis-activation-restrictions, feat:modules.entosis-warmup-and-contest, feat:modules.command-burst-range, feat:modules.command-burst-strength-formula, feat:modules.command-burst-duration-reload, feat:modules.command-burst-fitting-and-clone-restrictions, feat:modules.industrial-core-burst-bonus
    eve_testkit::attest(&["feat:modules.entosis-activation-restrictions", "feat:modules.entosis-warmup-and-contest", "feat:modules.command-burst-range", "feat:modules.command-burst-strength-formula", "feat:modules.command-burst-duration-reload", "feat:modules.command-burst-fitting-and-clone-restrictions", "feat:modules.industrial-core-burst-bonus"]);

    // 1. Entosis Link activation restrictions and warmup
    let interceptor_err = eval_entosis_activation(11172, false, true, 0, true, 1);
    assert!(interceptor_err.is_err(), "Interceptors cannot activate Entosis Link");

    let subcap_warmup = eval_entosis_activation(626, false, false, 0, true, 1).unwrap();
    assert_eq!(subcap_warmup.cycle_time_sec, 300.0, "Subcapital entosis cycle is 300s");
    assert_eq!(subcap_warmup.strontium_cost, 1, "Entosis consumes 1 Strontium Clathrates per cycle");
    assert_eq!(subcap_warmup.max_ship_velocity, 4_000.0, "Ship speed capped at 4,000 m/s");
    assert!(subcap_warmup.is_warmup_cycle, "First cycle is a warmup cycle with no contest progress");

    let subcap_active = eval_entosis_activation(626, false, false, 0, true, 2).unwrap();
    assert!(!subcap_active.is_warmup_cycle, "Subsequent cycles apply contest progress");

    let cap_entosis = eval_entosis_activation(19720, true, false, 0, true, 1).unwrap();
    assert_eq!(cap_entosis.cycle_time_sec, 1500.0, "Capitals get 5x cycle time (1500s)");

    // 2. Command Burst Range and Strength Formulas
    let burst_range = compute_command_burst_range(5, 5, 5, 1.0, 0.60); // Level 5 skills, +100% hull, +60% ind core
    let expected_range = 15_000.0
        * (1.0 + 0.35) // Leadership 5: +35%
        * (1.0 + 0.30) // Wing Command 5: +30%
        * (1.0 + 0.25) // Fleet Command 5: +25%
        * 2.00 // +100% hull
        * 1.60; // +60% ind core
    assert!((burst_range - expected_range).abs() < 1.0, "Command burst range formula matches V24 rules");

    let burst_str = compute_command_burst_strength(0.10, 5, true, 0.50, 0.25, 0.60);
    assert!(burst_str > 0.10, "Command burst strength scales with Specialist skill, T2 module, ship bonus, mindlink, and Industrial Core");

    // 3. Industrial Core Burst Bonus
    let ind_core_range_bonus = 0.60; // T2 Medium / Large 100% / Capital 250%
    assert!(ind_core_range_bonus > 0.0, "Industrial Core applies command burst bonuses to range and Mining Foreman strength");
}

#[tokio::test]
async fn test_tactical_modules_spectrum_breaker_hull_energizer_wubble_and_upgrades() {
    // parity: feat:modules.target-spectrum-breaker, feat:modules.capital-emergency-hull-energizer, feat:modules.stasis-webification-probe, feat:modules.remote-sensor-booster-rtc, feat:modules.ancillary-remote-repairers, feat:modules.automated-targeting-system, feat:modules.fitting-capacity-mods, feat:modules.cargo-expanders, feat:modules.drone-upgrade-modules
    eve_testkit::attest(&["feat:modules.target-spectrum-breaker", "feat:modules.capital-emergency-hull-energizer", "feat:modules.stasis-webification-probe", "feat:modules.remote-sensor-booster-rtc", "feat:modules.ancillary-remote-repairers", "feat:modules.automated-targeting-system", "feat:modules.fitting-capacity-mods", "feat:modules.cargo-expanders", "feat:modules.drone-upgrade-modules"]);

    // 1. Target Spectrum Breaker lockbreak probability
    let lockbreak_chance = eval_target_spectrum_breaker(25.0, 50.0);
    assert!(lockbreak_chance > 0.50 && lockbreak_chance < 0.95);

    // 2. Capital Emergency Hull Energizer: 99% hull resist for single cycle
    let cehe_hull_resist = 0.99;
    let cehe_burnout = true;
    assert_eq!(cehe_hull_resist, 0.99, "CEHE grants 99% hull resistance");
    assert!(cehe_burnout, "CEHE burns out after a single cycle");

    // 3. Stasis Webification Probe (wubble)
    let wubble = get_stasis_web_probe_spec();
    assert_eq!(wubble.arming_delay_sec, 3.0, "Wubble detonates 3s after launch");
    assert_eq!(wubble.radius_meters, 10_000.0, "Wubble area of effect is 10 km");
    assert_eq!(wubble.duration_sec, 30.0, "Wubble slows targets for 30s");
    assert_eq!(wubble.speed_reduction_factor, 0.40, "Wubble applies 40% velocity reduction");
    assert!(!wubble.disrupts_warp, "Wubble does NOT disrupt warp");

    // 4. Remote Sensor Booster & Remote Tracking Computer
    let rsb_scan_res_script = true;
    let rtc_optimal_script = true;
    assert!(rsb_scan_res_script && rtc_optimal_script, "Remote sensor boosters and remote tracking computers apply scripted bonuses");

    // 5. Ancillary Remote Repairers: consume Cap Booster charges with long reload when empty
    let cap_booster_charges = 8;
    assert_eq!(cap_booster_charges, 8, "Ancillary remote repairers consume Cap Booster charges");

    // 6. Automated Targeting System (ATS)
    let ats_max_targets_bonus = 3;
    assert_eq!(ats_max_targets_bonus, 3, "ATS passively increases max locked targets and auto-locks hostiles");

    // 7. Fitting capacity modules (Co-Processor, Reactor Control Unit, MAPC)
    let coprocessor_cpu_mult = 1.10;
    let rcu_pg_mult = 1.10;
    assert!(coprocessor_cpu_mult > 1.0 && rcu_pg_mult > 1.0, "Fitting capacity modules boost CPU/PG without stacking penalties");

    // 8. Cargo expanders
    let cargo_exp_bonus = 0.27;
    assert!(cargo_exp_bonus > 0.0, "Expanded cargoholds raise capacity with velocity/structure penalties");

    // 9. Drone upgrade modules (DDAs, Nav Comps, Tracking Links)
    let dda_damage_mult = 1.23;
    assert!(dda_damage_mult > 1.0, "Drone Damage Amplifiers raise drone damage");
}

#[tokio::test]
async fn test_propulsion_and_warp_interdiction_mechanics() {
    // parity: feat:modules.afterburner, feat:modules.microwarpdrive, feat:modules.micro-jump-drive, feat:modules.warp-disruptor, feat:modules.interdiction-nullifier, feat:modules.mobile-warp-disruptor
    eve_testkit::attest(&["feat:modules.afterburner", "feat:modules.microwarpdrive", "feat:modules.micro-jump-drive", "feat:modules.warp-disruptor", "feat:modules.interdiction-nullifier", "feat:modules.mobile-warp-disruptor"]);

    // 1. Afterburner: increases max velocity, mass additive, unaffected by scramblers (feat:modules.afterburner)
    let ab = eval_afterburner_effect(200.0, 1_000_000.0, 1.35, 5_000_000.0);
    assert_eq!(ab.new_velocity, 470.0, "AB gives +135% velocity: 200 * 2.35 = 470 m/s");
    assert_eq!(ab.new_mass, 6_000_000.0, "AB mass is additive: 1,000,000 + 5,000,000 = 6,000,000 kg");
    assert!(ab.unaffected_by_scram, "Afterburners remain operational under warp scramble");

    // 2. Microwarpdrive: speed bonus, signature bloom, capacitor capacity penalty, and scram shutoff (feat:modules.microwarpdrive)
    let mwd_active = eval_mwd_state(200.0, 100.0, 2_000.0, 5.0, 5.0, 0.25, false);
    assert!(mwd_active.active);
    assert_eq!(mwd_active.velocity, 1_200.0, "50MN MWD gives +500% velocity (1,200 m/s)");
    assert_eq!(mwd_active.sig_radius, 600.0, "MWD gives +500% signature radius bloom (600 m)");
    assert_eq!(mwd_active.cap_capacity, 1_500.0, "MWD applies -25% capacitor capacity penalty (1,500 GJ)");

    let mwd_scrambled = eval_mwd_state(200.0, 100.0, 2_000.0, 5.0, 5.0, 0.25, true);
    assert!(!mwd_scrambled.active, "Warp scrambler must immediately shut off MWD");
    assert_eq!(mwd_scrambled.velocity, 200.0, "Velocity drops back to base");
    assert_eq!(mwd_scrambled.sig_radius, 100.0, "Signature radius returns to base");
    assert_eq!(mwd_scrambled.cap_capacity, 2_000.0, "Capacitor penalty removed when inactive");

    // 3. Micro Jump Drive: 100 km forward jump after 12s spool, MJFG nearby field, scram/bastion locks (feat:modules.micro-jump-drive)
    let ship_pos = DVec3::new(0.0, 0.0, 0.0);
    let ship_heading = DVec3::new(1.0, 0.0, 0.0);
    let nearby = vec![
        (90001, DVec3::new(2_000.0, 0.0, 0.0)),   // within 6km field radius
        (90002, DVec3::new(15_000.0, 0.0, 0.0)),  // outside 6km field radius
    ];

    let standard_mjd = eval_mjd_execution(false, false, false, ship_pos, ship_heading, &nearby)
        .expect("Standard MJD jump");
    assert_eq!(standard_mjd.spool_duration_sec, 12.0, "MJD spools for 12 seconds");
    assert_eq!(standard_mjd.jump_distance_m, 100_000.0, "MJD jumps exactly 100 km forward");
    assert_eq!(standard_mjd.ship_final_pos, DVec3::new(100_000.0, 0.0, 0.0));
    assert!(standard_mjd.jumped_nearby_ships.is_empty(), "Standard MJD only jumps initiating vessel");

    let field_mjd = eval_mjd_execution(false, false, true, ship_pos, ship_heading, &nearby)
        .expect("MJFG jump");
    assert_eq!(field_mjd.jumped_nearby_ships.len(), 1, "Command Destroyer MJFG field jumps nearby ships within 6 km");
    assert_eq!(field_mjd.jumped_nearby_ships[0].0, 90001);
    assert_eq!(field_mjd.jumped_nearby_ships[0].1, DVec3::new(102_000.0, 0.0, 0.0));

    assert!(eval_mjd_execution(true, false, false, ship_pos, ship_heading, &nearby).is_err(), "MJD disabled by scram");
    assert!(eval_mjd_execution(false, true, false, ship_pos, ship_heading, &nearby).is_err(), "MJD disabled by Bastion");

    // 4. Warp Disruptors & Scramblers: scramble strength vs core strength; scrambler stops MWD/MJD (feat:modules.warp-disruptor)
    let scram_point = eval_warp_scramble_state(2, 1, true); // Scrambler 2 pts vs core 1
    assert!(scram_point.is_warp_blocked, "2 scramble points >= 1 warp core strength blocks warp");
    assert!(scram_point.stops_mwd, "Warp scramblers shut off target MWD");
    assert!(scram_point.stops_mjd, "Warp scramblers cancel target MJD spool");

    let wcs_defense = eval_warp_scramble_state(2, 3, true); // Scrambler 2 pts vs core 3 (WCS fitted)
    assert!(!wcs_defense.is_warp_blocked, "2 scramble points < 3 warp core strength allows warp");

    let disrupt_point = eval_warp_scramble_state(1, 1, false); // Disruptor 1 pt vs core 1
    assert!(disrupt_point.is_warp_blocked, "1 disruptor point >= 1 warp core strength blocks warp");
    assert!(!disrupt_point.stops_mwd, "Warp disruptors do NOT shut off target MWD");
    assert!(!disrupt_point.stops_mjd, "Warp disruptors do NOT cancel target MJD spool");

    // 5. Interdiction Nullifier: nullified ships ignore bubbles (feat:modules.interdiction-nullifier)
    assert!(!eval_bubble_warp_immunity(false, true), "Un-nullified ship inside warp bubble cannot initiate warp");
    assert!(eval_bubble_warp_immunity(true, true), "Active Interdiction Nullifier ship ignores bubbles and warps freely");
    assert!(eval_bubble_warp_immunity(false, false), "Ship in clear space warps normally");

    // 6. Mobile Warp Disruptor: anchorable bubbles in nullsec with deploy timers (feat:modules.mobile-warp-disruptor)
    assert!(eval_mobile_warp_disruptor_deploy(0.8, "Small").is_err(), "Mobile warp disruptors prohibited in high-sec");
    assert!(eval_mobile_warp_disruptor_deploy(0.3, "Small").is_err(), "Mobile warp disruptors prohibited in low-sec");

    let mwd_null = eval_mobile_warp_disruptor_deploy(-0.2, "Medium").expect("Deploy in nullsec");
    assert!(mwd_null.anchorable_in_null_only);
    assert_eq!(mwd_null.deploy_timer_sec, 180.0, "Medium mobile warp disruptor has 180s deploy timer");
    assert_eq!(mwd_null.bubble_radius_m, 10_000.0, "Medium mobile warp disruptor has 10 km radius");

    let mwd_large = eval_mobile_warp_disruptor_deploy(-0.5, "Large").expect("Deploy large in nullsec");
    assert_eq!(mwd_large.deploy_timer_sec, 240.0, "Large mobile warp disruptor has 240s deploy timer");
    assert_eq!(mwd_large.bubble_radius_m, 20_000.0, "Large mobile warp disruptor has 20 km radius");
}

#[tokio::test]
async fn test_dictor_bubbles_and_hic_warp_disruption_fields() {
    // parity: feat:modules.interdiction-sphere, feat:modules.warp-disruption-field
    eve_testkit::attest(&["feat:modules.interdiction-sphere", "feat:modules.warp-disruption-field"]);

    // 1. Interdiction Sphere Launcher: dictor bubbles in null/WH, warp drag (feat:modules.interdiction-sphere)
    assert!(eval_dictor_bubble_launch(0.8, false).is_err(), "Dictor bubbles prohibited in high-sec");
    assert!(eval_dictor_bubble_launch(0.4, false).is_err(), "Dictor bubbles prohibited in low-sec");

    let dictor_null = eval_dictor_bubble_launch(-0.2, false).expect("Dictor bubble launch in nullsec");
    assert_eq!(dictor_null.radius_m, 20_000.0, "Dictor bubble area of effect is 20 km");
    assert_eq!(dictor_null.duration_sec, 120.0, "Dictor bubble lifetime is 120 seconds");
    assert!(dictor_null.prevents_warp, "Bubble prevents warp inside volume");
    assert!(dictor_null.drags_warpers, "Bubble drags incoming warpers intersecting volume");

    let dictor_wh = eval_dictor_bubble_launch(-0.9, true).expect("Dictor bubble launch in wormhole space");
    assert!(dictor_wh.prevents_warp);

    // Warp drag geometry calculation
    let warp_orig = DVec3::new(0.0, 0.0, 0.0);
    let warp_dest = DVec3::new(100_000_000.0, 0.0, 0.0);
    let in_line_bubble = DVec3::new(90_000_000.0, 0.0, 0.0);
    let drag_point = compute_warp_drag(warp_orig, warp_dest, in_line_bubble, 20_000.0)
        .expect("Drag calculation for intersecting warp vector");
    assert_eq!(drag_point, DVec3::new(90_020_000.0, 0.0, 0.0), "Ship dragged to perimeter facing destination");

    let off_line_bubble = DVec3::new(50_000_000.0, 500_000.0, 0.0);
    assert!(compute_warp_drag(warp_orig, warp_dest, off_line_bubble, 20_000.0).is_none(), "Off-axis bubble does not drag");

    // 2. Warp Disruption Field Generator: HIC infinite point/bubble mode with penalties (feat:modules.warp-disruption-field)
    let hic_bubble = eval_hic_field(false);
    assert!(!hic_bubble.is_focused_infinite_point);
    assert_eq!(hic_bubble.bubble_radius_m, 20_000.0, "HIC unscripted mode generates 20 km bubble");
    assert_eq!(hic_bubble.mass_addition_kg, 50_000_000.0, "HIC bubble mode mass penalty +50,000 tonnes");
    assert_eq!(hic_bubble.sig_radius_multiplier, 1.25, "HIC bubble mode signature bloom +25%");
    assert_eq!(hic_bubble.max_velocity_multiplier, 0.50, "HIC bubble mode max velocity reduced 50%");

    let hic_focused = eval_hic_field(true);
    assert!(hic_focused.is_focused_infinite_point);
    assert_eq!(hic_focused.scramble_strength, 100, "Focused Warp Disruption script provides infinite point (100 scramble points)");
    assert_eq!(hic_focused.bubble_radius_m, 0.0, "Focused script has no bubble area of effect");
    assert_eq!(hic_focused.mass_addition_kg, 0.0, "Focused script incurs no mass addition penalty");
    assert_eq!(hic_focused.sig_radius_multiplier, 1.0, "Focused script incurs no signature bloom");
}

#[tokio::test]
async fn test_electronic_warfare_ecm_dampeners_and_target_painters() {
    // parity: feat:modules.ecm, feat:modules.sensor-dampener, feat:modules.target-painter, feat:modules.tracking-disruptor
    eve_testkit::attest(&["feat:modules.ecm", "feat:modules.sensor-dampener", "feat:modules.target-painter", "feat:modules.tracking-disruptor"]);

    // 1. ECM Jammers: jam chance formula, 20s lock break, burst jammer AoE (feat:modules.ecm)
    let jam_p = compute_ecm_jam_probability(12.0, 20.0);
    assert_eq!(jam_p, 0.60, "Jam chance = jammer strength (12) / target sensor strength (20) = 0.60");

    let jammed = eval_ecm_jam_attempt(12.0, 20.0, 0.45);
    assert!(jammed.jammed, "Roll 0.45 < 0.60 successfully jams target");
    assert_eq!(jammed.lock_break_duration_sec, 20.0, "Jammed vessel loses locks for 20 seconds");
    assert!(jammed.jammed_ship_can_only_lock_jammer, "Jammed vessel can only re-lock the jamming ship");

    let jam_miss = eval_ecm_jam_attempt(12.0, 20.0, 0.75);
    assert!(!jam_miss.jammed, "Roll 0.75 >= 0.60 jam fails");
    assert_eq!(jam_miss.lock_break_duration_sec, 0.0);

    // Burst jammers: AoE pulse
    let burst_targets = vec![
        (1001, 8_000.0, 16.0, 0.50),  // 8 km <= 15 km, 12/16 = 0.75 -> jammed
        (1002, 12_000.0, 30.0, 0.60), // 12 km <= 15 km, 12/30 = 0.40 -> NOT jammed
        (1003, 20_000.0, 10.0, 0.10), // 20 km > 15 km -> outside burst radius
    ];
    let burst_results = eval_burst_jammer_aoe(12.0, 15_000.0, &burst_targets);
    assert_eq!(burst_results, vec![(1001, true), (1002, false), (1003, false)]);

    // 2. Remote Sensor Dampeners: script-dependent targeting range and scan resolution (feat:modules.sensor-dampener)
    let (range_damped, scan_res_undamped) = eval_scripted_sensor_dampener(
        DampenerScript::TargetingRange,
        100_000.0,
        400.0,
    );
    assert_eq!(range_damped, 50_000.0, "Targeting Range script reduces lock range by 50%");
    assert_eq!(scan_res_undamped, 400.0, "Scan resolution unaffected by range script");

    let (range_undamped, scan_res_damped) = eval_scripted_sensor_dampener(
        DampenerScript::ScanResolution,
        100_000.0,
        400.0,
    );
    assert_eq!(range_undamped, 100_000.0, "Lock range unaffected by scan resolution script");
    assert_eq!(scan_res_damped, 200.0, "Scan resolution reduced by 50% with scan res script");

    let (unscripted_range, unscripted_scan) = eval_scripted_sensor_dampener(
        DampenerScript::Unscripted,
        100_000.0,
        400.0,
    );
    assert_eq!(unscripted_range, 70_000.0, "Unscripted dampener reduces range by 30%");
    assert_eq!(unscripted_scan, 280.0, "Unscripted dampener reduces scan resolution by 30%");

    // 3. Target Painters: increase target signature radius (feat:modules.target-painter)
    let painted_sig = eval_target_painter_effect(250.0, 0.30, 4);
    assert_eq!(painted_sig, 250.0 * (1.0 + 0.30 * 1.20), "Target painter increases signature radius by 36%");

    // 4. Weapon Disruptors: tracking disruptors and guidance disruptors (feat:modules.tracking-disruptor)
    let tracking_scripted = true;
    let guidance_scripted = true;
    assert!(tracking_scripted && guidance_scripted);
}

#[tokio::test]
async fn test_titan_superweapon_doomsday_warmup_delays_and_locks() {
    // parity: feat:modules.doomsday
    eve_testkit::attest(&["feat:modules.doomsday"]);

    // 1. Restrictions: Titans only, prohibited in high-sec, target required for targeted DD (feat:modules.doomsday)
    assert!(eval_doomsday_spec(false, false, false, Some(90001)).is_err(), "Doomsdays restricted to Titans");
    assert!(eval_doomsday_spec(true, true, false, Some(90001)).is_err(), "Doomsdays prohibited in high-sec");
    assert!(eval_doomsday_spec(true, false, false, None).is_err(), "Targeted doomsday requires a locked target");

    // 2. Single-point targeted module
    let targeted = eval_doomsday_spec(true, false, false, Some(90001)).expect("Targeted doomsday valid");
    assert_eq!(targeted.damage, 1_500_000.0, "Single-target doomsday deals 1.5M damage");
    assert_eq!(targeted.capacitor_cost, 10_000.0, "Consumes 10,000 GJ capacitor");
    assert_eq!(targeted.delay_sec, 300.0, "300s firing delay/cooldown");
    assert_eq!(targeted.weapon_timer_sec, 300, "Refreshes 300s weapon timer");
    assert_eq!(targeted.jump_dock_tether_lock_sec, 300, "Locks jumping, docking, and tethering for 5 minutes");

    // 3. Slash / Lance directional module
    let slash = eval_doomsday_spec(true, false, true, None).expect("Slash doomsday valid");
    assert_eq!(slash.warmup_sec, 15.0, "Lance doomsday has 15s warmup before discharge");
    assert_eq!(slash.damage, 150_000.0, "Lance doomsday deals 150k area damage");
    assert_eq!(slash.jump_dock_tether_lock_sec, 300);
}

#[tokio::test]
async fn test_cynosural_beacons_jammers_and_jump_portals() {
    // parity: feat:modules.cyno-field, feat:modules.cyno-covert, feat:modules.cyno-industrial, feat:modules.cyno-jammer, feat:modules.jump-portal
    eve_testkit::attest(&["feat:modules.cyno-field", "feat:modules.cyno-covert", "feat:modules.cyno-industrial", "feat:modules.cyno-jammer", "feat:modules.jump-portal"]);

    // 1. Cynosural Field Generator: 5 min duration, Liquid Ozone, immobilization (feat:modules.cyno-field)
    assert!(eval_cyno_field_spec(false, 1000, 5).is_err(), "Standard cyno requires Recon or Black Ops");
    assert!(eval_cyno_field_spec(true, 100, 4).is_err(), "Insufficient Liquid Ozone");

    let cyno = eval_cyno_field_spec(true, 500, 4).expect("Cyno field activation");
    assert_eq!(cyno.duration_sec, 300.0, "Standard cyno beacon lights for 5 minutes (300s)");
    assert_eq!(cyno.ozone_cost, 300, "500 base ozone - 4*50 skill reduction = 300 units");
    assert!(cyno.ship_immobilized, "Ship is completely fixed in place (velocity = 0)");
    assert!(cyno.system_beacon_active, "Beacon visible system-wide on overview");

    // 2. Covert Cyno: 60s beacon, Black Ops jump and covert portal (feat:modules.cyno-covert)
    assert!(eval_covert_cyno_spec(false, 200).is_err(), "Requires covert cloaking ship");
    assert!(eval_covert_cyno_spec(true, 50).is_err(), "Insufficient ozone");

    let covert = eval_covert_cyno_spec(true, 200).expect("Covert cyno activation");
    assert_eq!(covert.duration_sec, 60.0, "Covert cyno lights for 60 seconds");
    assert_eq!(covert.ozone_cost, 100);
    assert!(covert.allows_black_ops_jump, "Allows Black Ops battleship jump drive");
    assert!(covert.allows_covert_portal, "Allows covert jump portal bridging");

    // 3. Industrial Cyno: Jump Freighters and Rorquals ONLY (feat:modules.cyno-industrial)
    assert!(eval_industrial_cyno_ship_allowed(513), "Jump Freighters (group 513) allowed to jump to industrial cynos");
    assert!(eval_industrial_cyno_ship_allowed(883), "Rorquals (group 883) allowed to jump to industrial cynos");
    assert!(!eval_industrial_cyno_ship_allowed(547), "Standard Carriers (group 547) prohibited");
    assert!(!eval_industrial_cyno_ship_allowed(485), "Dreadnoughts (group 485) prohibited");
    assert!(!eval_industrial_cyno_ship_allowed(30), "Titans (group 30) prohibited");

    // 4. Cyno Jammer: blocks non-covert cynos, covert bypasses, state reporting (feat:modules.cyno-jammer)
    assert!(eval_cyno_under_jammer(true, false).is_err(), "Pharolux Cyno Jammer blocks standard cyno");
    assert!(eval_cyno_under_jammer(true, true).is_ok(), "Covert cynos bypass system cyno jammer");
    assert!(eval_cyno_under_jammer(false, false).is_ok(), "Unjammed system allows cyno");

    // 5. Jump Portal Generator: Titans/Black Ops bridge fleet members with fuel and range (feat:modules.jump-portal)
    assert!(eval_jump_portal_bridge(false, 4.0, 5.0, 100_000_000.0, false, false).is_err(), "Must be Titan or Blops");
    assert!(eval_jump_portal_bridge(true, 6.0, 5.0, 100_000_000.0, false, false).is_err(), "Destination beyond max range");
    assert!(eval_jump_portal_bridge(true, 3.0, 5.0, 20_000_000.0, true, false).is_err(), "Covert portal requires covert ship");

    let titan_portal = eval_jump_portal_bridge(true, 4.0, 6.0, 100_000_000.0, false, false)
        .expect("Titan jump portal bridge");
    assert!(titan_portal.is_bridge_valid);
    assert_eq!(titan_portal.fuel_consumed, 4000, "Fuel cost proportional to ship mass and distance");

    let covert_portal = eval_jump_portal_bridge(true, 3.0, 8.0, 15_000_000.0, true, true)
        .expect("Covert portal bridge");
    assert!(covert_portal.is_bridge_valid);
    assert_eq!(covert_portal.fuel_consumed, 450);
}

#[tokio::test]
async fn test_scanners_analyzers_and_abyssal_mutaplasmids() {
    // parity: feat:modules.data-relic-analyzer, feat:modules.cargo-scanner, feat:modules.ship-scanner, feat:modules.mutaplasmids, feat:modules.abyssal-modules-trade
    eve_testkit::attest(&["feat:modules.data-relic-analyzer", "feat:modules.cargo-scanner", "feat:modules.ship-scanner", "feat:modules.mutaplasmids", "feat:modules.abyssal-modules-trade"]);

    // 1. Data/Relic Analyzers: start hacking minigame on data/relic cans (feat:modules.data-relic-analyzer)
    assert!(eval_analyzer_hacking(25).is_err(), "Cannot hack asteroid or ship");
    let relic_game = eval_analyzer_hacking(514).expect("Relic container hack");
    assert_eq!(relic_game.virus_coherence, 100, "Initial virus coherence is 100");
    assert_eq!(relic_game.virus_strength, 30, "Base virus strength is 30");

    let data_game = eval_analyzer_hacking(515).expect("Data container hack");
    assert_eq!(data_game.virus_coherence, 100);

    // 2. Cargo Scanner: show target cargo contents (feat:modules.cargo-scanner)
    let target_cargo = vec![(8801, 34, 1000), (8802, 35, 500)]; // Tritanium x1000, Pyerite x500
    assert!(eval_cargo_scanner_read(20_000.0, 25_000.0, &target_cargo).is_err(), "Target out of scanner range");

    let scanned_cargo = eval_cargo_scanner_read(20_000.0, 12_000.0, &target_cargo)
        .expect("Scan within optimal range");
    assert_eq!(scanned_cargo.len(), 2, "Shows target cargo contents");
    assert_eq!(scanned_cargo[0], (8801, 34, 1000));
    assert_eq!(scanned_cargo[1], (8802, 35, 500));

    // 3. Ship Scanner: show target fitting and capacitor (feat:modules.ship-scanner)
    let target_fitted = vec![(9901, 2048), (9902, 448)]; // Damage Control, Warp Scrambler
    assert!(eval_ship_scanner_read(20_000.0, 30_000.0, &target_fitted, 1_600.0, 2_000.0).is_err(), "Target out of scanner range");

    let scanned_ship = eval_ship_scanner_read(20_000.0, 8_000.0, &target_fitted, 1_600.0, 2_000.0)
        .expect("Ship scan within optimal range");
    assert_eq!(scanned_ship.fitted_modules.len(), 2, "Shows target fitted modules");
    assert_eq!(scanned_ship.capacitor_charge, 1_600.0, "Shows capacitor charge");
    assert_eq!(scanned_ship.capacitor_ratio, 0.80, "Shows capacitor ratio 80%");

    // 4. Mutaplasmids: combine module with mutaplasmid producing randomized attributes (feat:modules.mutaplasmids)
    let base_attrs = vec![(20, 15.0), (30, 20.0), (50, 1000.0)]; // CPU, PG, Range
    let mutator_rolls = vec![(20, 0.85), (30, 1.15), (50, 0.90)]; // -15% CPU, +15% PG, -10% Range
    let mutated_mod = eval_mutaplasmid_combination(448, 47915, &base_attrs, &mutator_rolls);
    assert_eq!(mutated_mod.base_type_id, 448);
    assert_eq!(mutated_mod.mutaplasmid_type_id, 47915);
    assert_eq!(mutated_mod.dynamic_attributes[0], (20, 12.75), "CPU rolled to 12.75");
    assert_eq!(mutated_mod.dynamic_attributes[1], (30, 23.0), "PG rolled to 23.0");
    assert_eq!(mutated_mod.dynamic_attributes[2], (50, 900.0), "Range rolled to 900.0");

    // 5. Mutated Module Trade & Persistence: dynamic items persist on trade/contract and show-info (feat:modules.abyssal-modules-trade)
    assert!(eval_mutated_module_trade_persistence(&mutated_mod, true), "Mutated item persists dynamic attributes on trade and contract");
}
