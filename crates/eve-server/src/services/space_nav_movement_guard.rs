//! Unified ship movement validation guard (`#591`, `[Arch-09]`).
//!
//! Centralizes pre-movement checks across warp, bookmark-warp, stargate,
//! Ansiblex, wormhole, cynosural jump/bridge/conduit, shipcaster, filament,
//! acceleration-gate, and reconnect return-warp entry points:
//! 1. Unified tackle/bubble check via `Ballpark::is_ball_scrambled` (no direct
//!    calls to `CombatState::is_warp_scrambled()` outside `eve-destiny`).
//! 2. Immobilizing modules (Siege, Bastion, Triage, Industrial Core),
//!    active Cynosural / Entosis / Skyhook / CRAB locks, Doomsday / Disruptive
//!    Lance firing & debuff locks (`lance_disrupted_remaining_s > 0.0`), and
//!    piloted Upwell structures (`active_ship_id == structure_id` or in `snap.structures`).
//! 3. Cloak rules (non-covert cloak blocks warp initiation).
//! 4. Hull class, capsule (`670`), freighter (`513 | 902`), capital, and mass limits.
//! 5. System & location locks (Abyssal space, Zarzakh Horizon Siege / jump
//!    drive ban, wormhole-space jump drive ban).

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::space_warp_jump::SOLAR_SYSTEM_ZARZAKH;
use eve_destiny::{Ball, CombatState, MovementMode};
use eve_proto::BridgeRpcError;

/// Category of acceleration gate for `MovementIntent::AccelerationGate`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccelerationGateKind {
    Generic,
    FwComplex,
    InsurgencyOutpost,
    EssBank,
    AnomicBurner,
    Dungeon,
    DedOrExploration,
    VoidExit,
}

/// Logical ship movement intent validated by [`ServerContext::validate_ship_movement`].
#[derive(Debug, Clone)]
pub enum MovementIntent {
    /// Standard or autopilot warp (`CmdWarpToStuff`, `CmdWarpToStuffAutopilot`).
    Warp { sys_id: u32 },
    /// Bookmark warp (`CmdGotoBookmark` when distance >= 150 km).
    BookmarkWarp { sys_id: u32 },
    /// Agency content warp (`ExecuteContentWarp`).
    AgencyWarp { sys_id: u32 },
    /// Scripted/client vector warp (`warpVectorMgr.UseVector`).
    VectorWarp { sys_id: u32 },
    /// Stargate jump (`CmdStargateJump`).
    StargateJump {
        from_sys: u32,
        to_sys: u32,
        gate_id: i64,
    },
    /// Ansiblex jump bridge (`CmdJumpThroughStructureStargate`).
    AnsiblexJump { from_sys: u32, to_sys: u32 },
    /// Wormhole jump (`WormholeJump` / cosmic / Pochven WH).
    WormholeJump {
        from_sys: u32,
        max_jump_mass: Option<f64>,
    },
    /// Cynosural / Jump Drive / Pharolux Beacon jump.
    CynoJump {
        from_sys: u32,
        to_sys: u32,
        allow_bubbled: bool,
    },
    /// Titan / Black Ops bridge portal (`BridgeToCyno`).
    CynoBridge {
        from_sys: u32,
        to_sys: u32,
        allow_bubbled: bool,
    },
    /// Covert or Fleet Conduit jump (`ConduitJump`).
    ConduitJump {
        from_sys: u32,
        to_sys: u32,
        allow_bubbled: bool,
    },
    /// Empire Shipcaster jump.
    ShipcasterJump { from_sys: u32, to_sys: u32 },
    /// Needlejack / Pochven random jump filament (`ActivateRandomJumpFilament`).
    RandomFilament { from_sys: u32 },
    /// Abyssal Deadspace, Proving Grounds, or Void Space filament.
    AbyssalOrVoidFilament {
        from_sys: u32,
        reject_when_cloaked: bool,
        reject_when_tethered: bool,
    },
    /// Acceleration gate activation (`ActivateAccelerationGate`).
    AccelerationGate {
        sys_id: u32,
        kind: AccelerationGateKind,
    },
}

/// Snapshot returned after [`ServerContext::validate_ship_movement`] succeeds.
#[derive(Debug, Clone)]
pub struct ValidatedShipMovement {
    pub sys_id: u32,
    pub ball: Option<Ball>,
    pub combat_state: Option<CombatState>,
    pub ship_type_id: u32,
    pub ship_group_id: u32,
    pub is_covert_cloaked: bool,
}

impl MovementIntent {
    pub fn sys_id(&self) -> u32 {
        match *self {
            Self::Warp { sys_id }
            | Self::BookmarkWarp { sys_id }
            | Self::AgencyWarp { sys_id }
            | Self::VectorWarp { sys_id }
            | Self::AccelerationGate { sys_id, .. } => sys_id,
            Self::StargateJump { from_sys, .. }
            | Self::AnsiblexJump { from_sys, .. }
            | Self::WormholeJump { from_sys, .. }
            | Self::CynoJump { from_sys, .. }
            | Self::CynoBridge { from_sys, .. }
            | Self::ConduitJump { from_sys, .. }
            | Self::ShipcasterJump { from_sys, .. }
            | Self::RandomFilament { from_sys }
            | Self::AbyssalOrVoidFilament { from_sys, .. } => from_sys,
        }
    }

    fn action_label(&self) -> &'static str {
        match self {
            Self::Warp { .. }
            | Self::BookmarkWarp { .. }
            | Self::AgencyWarp { .. }
            | Self::VectorWarp { .. } => "warp",
            Self::StargateJump { .. } | Self::AnsiblexJump { .. } => "jump",
            Self::WormholeJump { .. } => "jump wormhole",
            Self::CynoJump { .. }
            | Self::CynoBridge { .. }
            | Self::ConduitJump { .. }
            | Self::ShipcasterJump { .. } => "jump",
            Self::RandomFilament { .. } | Self::AbyssalOrVoidFilament { .. } => "activate filament",
            Self::AccelerationGate { .. } => "activate acceleration gate",
        }
    }

    fn requires_ball_in_space(&self) -> bool {
        !matches!(
            self,
            Self::AccelerationGate {
                kind: AccelerationGateKind::AnomicBurner
                    | AccelerationGateKind::DedOrExploration
                    | AccelerationGateKind::VoidExit,
                ..
            } | Self::AbyssalOrVoidFilament { .. }
        )
    }

    fn checks_bubble_and_scram(&self) -> bool {
        !matches!(
            self,
            Self::StargateJump { .. } | Self::AnsiblexJump { .. } | Self::WormholeJump { .. }
        )
    }

    fn allow_bubbled(&self) -> bool {
        match *self {
            Self::CynoJump { allow_bubbled, .. }
            | Self::CynoBridge { allow_bubbled, .. }
            | Self::ConduitJump { allow_bubbled, .. } => allow_bubbled,
            _ => false,
        }
    }

    fn is_warp_intent(&self) -> bool {
        matches!(
            self,
            Self::Warp { .. }
                | Self::BookmarkWarp { .. }
                | Self::AgencyWarp { .. }
                | Self::VectorWarp { .. }
        )
    }
}

fn is_piloted_upwell_structure(ctx: &ServerContext, ship_id: i64) -> bool {
    if crate::services::structure_control_ops::is_structure_vessel(ctx, ship_id) {
        return true;
    }
    ctx.db.with_snapshot(|snap| {
        snap.structures.contains_key(&ship_id)
            || snap.characters.values().any(|ch| {
                ch.active_ship_id == ship_id
                    && (snap.structures.contains_key(&ch.active_ship_id)
                        || snap.items.get(&ship_id).is_some_and(|item| {
                            ctx.sde
                                .types
                                .get(&item.type_id)
                                .is_some_and(|t| t.category_id == 65)
                        }))
            })
    })
}

/// Validates piloted Upwell structure control and Category 5 system-level locks
/// before touching Ballpark state.
fn validate_system_movement_locks(
    ctx: &ServerContext,
    ship_id: i64,
    intent: &MovementIntent,
) -> Result<(), BridgeRpcError> {
    let action = intent.action_label();
    if is_piloted_upwell_structure(ctx, ship_id) {
        return Err(BridgeRpcError {
            code: "StructureCannotNavigate".into(),
            message: format!(
                "Cannot {action} while controlling an anchored Upwell structure ({ship_id})"
            ),
        });
    }

    // Category 5: System-level locks before touching Ballpark state.
    match *intent {
        MovementIntent::Warp { sys_id }
        | MovementIntent::BookmarkWarp { sys_id }
        | MovementIntent::AgencyWarp { sys_id }
        | MovementIntent::VectorWarp { sys_id } => {
            if (32_000_000..=32_999_999).contains(&sys_id) {
                return Err(BridgeRpcError {
                    code: "CannotWarpInAbyssalSpace".into(),
                    message: "Warp navigation is disabled inside Abyssal Deadspace".into(),
                });
            }
        }
        MovementIntent::StargateJump {
            from_sys, to_sys, ..
        } => {
            if (from_sys == SOLAR_SYSTEM_ZARZAKH || to_sys == SOLAR_SYSTEM_ZARZAKH)
                && crate::services::cluster_macho_misc_services::is_zarzakh_horizon_siege_active(
                    ctx,
                )
            {
                return Err(BridgeRpcError::user_error("HorizonSiegeGateAccessDenied"));
            }
        }
        MovementIntent::CynoJump {
            from_sys, to_sys, ..
        }
        | MovementIntent::CynoBridge {
            from_sys, to_sys, ..
        }
        | MovementIntent::ConduitJump {
            from_sys, to_sys, ..
        } => {
            if (31_000_000..32_000_000).contains(&from_sys)
                || (31_000_000..32_000_000).contains(&to_sys)
            {
                return Err(BridgeRpcError {
                    code: "JumpDriveForbiddenInWormholeSpace".into(),
                    message:
                        "Jump drive and cynosural beacon operations are prohibited in wormhole space"
                            .into(),
                });
            }
            if from_sys == SOLAR_SYSTEM_ZARZAKH || to_sys == SOLAR_SYSTEM_ZARZAKH {
                return Err(BridgeRpcError {
                    code: "JumpDriveForbiddenInZarzakh".into(),
                    message: "Jump drives are disabled inside Zarzakh".into(),
                });
            }
        }
        _ => {}
    }

    Ok(())
}

/// Category 2: Navigation locks (Cyno, Doomsday/Lance, Entosis, Skyhook, CRAB)
/// and immobilizing modules (Siege, Bastion, Triage, Industrial Core).
fn validate_navigation_and_immobilizing_locks(
    ctx: &ServerContext,
    ship_id: i64,
    intent: &MovementIntent,
) -> Result<(), BridgeRpcError> {
    let action = intent.action_label();
    let nav_action = match intent {
        MovementIntent::AccelerationGate {
            kind: AccelerationGateKind::FwComplex | AccelerationGateKind::EssBank,
            ..
        } => "warp",
        _ => action,
    };
    if let Err(err) = crate::services::tactical_modules::check_ship_navigation_lock(
        ctx, ship_id, nav_action,
    ) {
        if matches!(
            intent,
            MovementIntent::AccelerationGate {
                kind: AccelerationGateKind::FwComplex,
                ..
            }
        ) {
            return Err(BridgeRpcError {
                code: "ShipNavigationLocked".into(),
                message: err.message,
            });
        }
        return Err(err);
    }

    if let Err(err) = crate::services::space_navigation_advanced::check_immobilizing_module_active(
        ctx, ship_id, nav_action,
    ) {
        let code = match intent {
            MovementIntent::AccelerationGate {
                kind: AccelerationGateKind::FwComplex,
                ..
            } => "ImmobilizingModuleActive",
            MovementIntent::WormholeJump { .. } => "CannotJumpWormholeWhileImmobilized",
            _ => err.code.as_str(),
        };
        return Err(BridgeRpcError {
            code: code.into(),
            message: err.message,
        });
    }

    Ok(())
}

/// Validates system-level locks, navigation-lock modules, immobilizing modules,
/// and piloted Upwell structure control before resolving a warp target.
pub(crate) fn validate_pre_movement_locks(
    ctx: &ServerContext,
    ship_id: i64,
    intent: &MovementIntent,
) -> Result<(), BridgeRpcError> {
    validate_system_movement_locks(ctx, ship_id, intent)?;
    validate_navigation_and_immobilizing_locks(ctx, ship_id, intent)
}

/// Sync helper for reconnect return-warp eligibility while avoiding `CombatState::is_warp_scrambled`.
pub(crate) fn check_sync_pre_warp_eligibility(ctx: &ServerContext, ship_id: i64) -> bool {
    validate_pre_movement_locks(
        ctx,
        ship_id,
        &MovementIntent::Warp { sys_id: 0 },
    )
    .is_ok()
}

/// Sync check executed while holding a `Ballpark` reference (e.g. `reconnect_character_in_space_sync`).
pub(crate) fn can_warp_in_ballpark(bp: &eve_destiny::Ballpark, ship_id: i64) -> bool {
    if bp.is_ball_scrambled(ship_id) {
        return false;
    }
    if let Some(cs) = bp.combat_states.get(&ship_id) {
        if cs.lance_disrupted_remaining_s > 0.0 {
            return false;
        }
        if cs.is_cloaked && cs.gate_cloak_remaining_ticks == 0 && !cs.is_covert_cloak {
            return false;
        }
    }
    true
}

impl ServerContext {
    /// Unified pre-movement validator covering tackle/bubbles, immobilizing modules,
    /// cloak rules, hull/mass limits, and system/location locks (`#591`).
    pub async fn validate_ship_movement(
        &self,
        _char_id: i64,
        ship_id: i64,
        intent: MovementIntent,
    ) -> Result<ValidatedShipMovement, BridgeRpcError> {
        validate_system_movement_locks(self, ship_id, &intent)?;

        let sys_id = intent.sys_id();
        let sys_handle = self.systems.get_or_spawn(sys_id);

        let ball_opt = sys_handle
            .get_ball_snapshot(ship_id)
            .await
            .map_err(|e| BridgeRpcError {
                code: "DestinyQueryFailed".into(),
                message: e,
            })?
            .map(|(_, b)| b);

        if intent.requires_ball_in_space() && ball_opt.is_none() {
            return Err(BridgeRpcError {
                code: "ShipNotInSpace".into(),
                message: "Active ship is not spawned in space".into(),
            });
        }

        let cs_opt = sys_handle.get_combat_state(ship_id).await.ok().flatten();

        // Reject if already in active warp (or aligning only where allowed).
        if let Some(ref ball) = ball_opt {
            if intent.is_warp_intent() {
                if matches!(intent, MovementIntent::BookmarkWarp { .. }) {
                    if matches!(ball.mode, MovementMode::Warp { .. }) {
                        return Err(BridgeRpcError {
                            code: "ShipNotInSpace".into(),
                            message: "Active ship is already in warp".into(),
                        });
                    }
                } else {
                    crate::services::nav_warp_interdiction::ensure_not_in_active_warp(
                        &ball.mode,
                    )?;
                }
            } else if matches!(ball.mode, MovementMode::Warp { .. }) {
                let code = match intent {
                    MovementIntent::RandomFilament { .. } => "CannotActivateFilamentInWarp",
                    _ => "ShipInWarp",
                };
                return Err(BridgeRpcError {
                    code: code.into(),
                    message: "Cannot perform movement while ship is in warp".into(),
                });
            }
        }

        // Explicit Disruptive Lance debuff sets `warp_scramble_strength = max(100)` internally,
        // so check it before generic `is_ball_scrambled` so `DisruptiveLanceDebuffActive` is not shadowed by `WarpScrambled`.
        if let Some(ref cs) = cs_opt {
            if cs.lance_disrupted_remaining_s > 0.0 {
                let code = match intent {
                    MovementIntent::AccelerationGate {
                        kind: AccelerationGateKind::FwComplex,
                        ..
                    } => "ShipNavigationLocked",
                    _ => "DisruptiveLanceDebuffActive",
                };
                return Err(BridgeRpcError {
                    code: code.into(),
                    message: format!(
                        "Cannot {} while disrupted by a Superweapon Lance ({:.1} s remaining)",
                        intent.action_label(),
                        cs.lance_disrupted_remaining_s
                    ),
                });
            }
        }

        // Category 1: Tackle and warp-disruption bubble interdiction via `Ballpark::is_ball_scrambled`.
        // Checked before Category 2 navigation locks so active scram/WCS < 0 surfaces `WarpScrambled` first.
        if intent.checks_bubble_and_scram() && ball_opt.is_some() {
            let allow_bubbled = intent.allow_bubbled();
            let scrambled = if allow_bubbled {
                // Bubbles allowed (e.g. covert portal bridging target), but targeted warp
                // scrambling (`warp_scramble_strength > 0 && >= warp_core_strength`) or
                // negative warp core strength (`warp_core_strength < 0`) still blocks!
                cs_opt.as_ref().is_some_and(|cs| {
                    cs.warp_core_strength < 0
                        || (cs.warp_scramble_strength > 0
                            && cs.warp_scramble_strength >= cs.warp_core_strength)
                })
            } else {
                sys_handle
                    .is_ball_scrambled(ship_id)
                    .await
                    .unwrap_or_else(|_| sys_handle.is_ball_scrambled_sync(ship_id))
            };
            if scrambled {
                let code = match intent {
                    MovementIntent::RandomFilament { .. } => "ShipWarpScrambled",
                    _ => "WarpScrambled",
                };
                return Err(BridgeRpcError {
                    code: code.into(),
                    message: "WarpScrambled: movement blocked by warp scrambler or warp disruption bubble".into(),
                });
            }
        }

        // Category 2: Navigation locks (Cyno, Doomsday/Lance, Entosis, Skyhook, CRAB) and immobilizing modules.
        validate_navigation_and_immobilizing_locks(self, ship_id, &intent)?;

        // Category 3: Cloak & tether rules.
        let mut is_covert_cloaked = false;
        if let Some(ref cs) = cs_opt {
            if intent.is_warp_intent() {
                if cs.is_cloaked
                    && cs.gate_cloak_remaining_ticks == 0
                    && !cs.is_covert_cloak
                {
                    return Err(BridgeRpcError {
                        code: "CloakedCannotWarp".into(),
                        message: "Cannot initiate warp while cloaked without a Covert Ops Cloaking Device".into(),
                    });
                }
                is_covert_cloaked = cs.is_cloaked
                    && cs.is_covert_cloak
                    && cs.gate_cloak_remaining_ticks == 0;
            } else if let MovementIntent::AbyssalOrVoidFilament {
                reject_when_cloaked,
                reject_when_tethered,
                ..
            } = intent
            {
                if reject_when_cloaked && cs.is_cloaked {
                    return Err(BridgeRpcError {
                        code: "ShipIsCloaked".into(),
                        message: "Cannot activate filament while cloaked".into(),
                    });
                }
                if reject_when_tethered && cs.is_tethered {
                    return Err(BridgeRpcError {
                        code: "ShipIsTethered".into(),
                        message: "Cannot activate filament while tethered".into(),
                    });
                }
            }
        }

        // Resolve canonical ship type / group / mass from Ballpark + DB + SDE.
        let (db_type_id, ball_type_id, ball_mass) = {
            let db_tid = self
                .db
                .with_snapshot(|snap| snap.items.get(&ship_id).map(|i| i.type_id))
                .unwrap_or(0);
            let b_tid = ball_opt.as_ref().map(|b| b.type_id).unwrap_or(0);
            let b_mass = ball_opt.as_ref().map(|b| b.mass).unwrap_or(0.0);
            (db_tid, b_tid, b_mass)
        };
        let ship_type_id = if ball_type_id != 0 {
            ball_type_id
        } else {
            db_type_id
        };
        let sde_type = self.sde.types.get(&ship_type_id);
        let ship_group_id = sde_type.map(|t| t.group_id).unwrap_or(0);
        let effective_mass = if ball_mass > 0.0 {
            ball_mass
        } else {
            sde_type.map(|t| t.mass).unwrap_or(0.0)
        };

        // Category 4: Hull class, capsule, freighter, capital, and mass restrictions.
        match intent {
            MovementIntent::StargateJump {
                from_sys,
                to_sys,
                gate_id,
            } => {
                let is_restricted_capital =
                    matches!(ship_group_id, 30 | 485 | 547 | 659 | 883 | 1538 | 4594);
                let is_zarzakh_gate =
                    from_sys == SOLAR_SYSTEM_ZARZAKH || to_sys == SOLAR_SYSTEM_ZARZAKH;
                let is_orca = ship_type_id == 28_606 || ship_group_id == 941;
                let is_freighter = matches!(ship_group_id, 513 | 902);
                if is_zarzakh_gate && !is_orca && (is_restricted_capital || is_freighter) {
                    return Err(BridgeRpcError {
                        code: "ShipCannotUseStargate".into(),
                        message: format!(
                            "Capital ship or freighter type {ship_type_id} (group {ship_group_id}) cannot jump stargate {gate_id}"
                        ),
                    });
                }
                let is_pochven_gate = self
                    .sde
                    .solar_systems
                    .get(&from_sys)
                    .map(|s| s.region_id)
                    == Some(10_000_070)
                    || self
                        .sde
                        .solar_systems
                        .get(&to_sys)
                        .map(|s| s.region_id)
                        == Some(10_000_070);
                let is_highsec_gate = self
                    .sde
                    .solar_systems
                    .get(&from_sys)
                    .is_some_and(|s| s.security_status >= 0.45)
                    || self
                        .sde
                        .solar_systems
                        .get(&to_sys)
                        .is_some_and(|s| s.security_status >= 0.45);
                let is_supercapital = matches!(ship_group_id, 30 | 659);
                if (is_pochven_gate && (is_restricted_capital || is_freighter))
                    || is_supercapital
                    || (is_highsec_gate && is_restricted_capital)
                {
                    return Err(BridgeRpcError {
                        code: "CapitalShipCannotJumpStargate".into(),
                        message: format!(
                            "Capital ship or freighter type {ship_type_id} (group {ship_group_id}) cannot jump stargate {gate_id}"
                        ),
                    });
                }
            }
            MovementIntent::WormholeJump { max_jump_mass, .. } => {
                if matches!(ship_group_id, 30 | 659) {
                    return Err(BridgeRpcError {
                        code: "ShipRestrictedFromWormhole".into(),
                        message: format!(
                            "Ship type {ship_type_id} (group {ship_group_id}) is restricted from entering wormholes (Supercarrier/Titan prohibition)"
                        ),
                    });
                }
                if let Some(max_mass) = max_jump_mass {
                    if effective_mass > max_mass {
                        return Err(BridgeRpcError {
                            code: "ShipExceedsWormholeJumpMass".into(),
                            message: format!(
                                "Ship mass ({effective_mass:.0} kg) exceeds wormhole single-jump mass limit ({max_mass:.0} kg)"
                            ),
                        });
                    }
                }
            }
            MovementIntent::AnsiblexJump { .. } => {
                if effective_mass > 1_480_000_000.0 || matches!(ship_group_id, 30 | 659) {
                    return Err(BridgeRpcError {
                        code: "ShipNotAllowedThroughAnsiblex".into(),
                        message: "Ship exceeds Ansiblex mass limit of 1.48 Mt".into(),
                    });
                }
            }
            MovementIntent::AccelerationGate { kind, .. } => {
                if matches!(kind, AccelerationGateKind::Generic) {
                    let ship_rig_size = sde_type
                        .and_then(|t| t.default_attributes.get(&1547).copied())
                        .unwrap_or(0.0);
                    if ship_rig_size >= 4.0
                        || effective_mass > 500_000_000.0
                        || matches!(ship_group_id, 30 | 485 | 547 | 659 | 883 | 1538 | 4594)
                    {
                        return Err(BridgeRpcError {
                            code: "ShipTooLargeForGate".into(),
                            message: "Capital ships cannot activate this acceleration gate"
                                .into(),
                        });
                    }
                } else if matches!(kind, AccelerationGateKind::AnomicBurner) {
                    if matches!(ship_group_id, 27 | 30 | 419 | 485 | 547 | 659 | 898 | 900)
                        || effective_mass >= 50_000_000.0
                    {
                        return Err(BridgeRpcError {
                            code: "ShipRestrictedByGate".into(),
                            message: format!(
                                "Ship type {ship_type_id} (group {ship_group_id}) is too large for this Anomic Burner acceleration gate"
                            ),
                        });
                    }
                }
            }
            _ => {}
        }

        Ok(ValidatedShipMovement {
            sys_id,
            ball: ball_opt,
            combat_state: cs_opt,
            ship_type_id,
            ship_group_id,
            is_covert_cloaked,
        })
    }
}

#[cfg(test)]
#[path = "space_nav_movement_guard_tests.rs"]
mod space_nav_movement_guard_tests;
