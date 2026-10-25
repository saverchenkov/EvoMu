//! `eve-dogma` — EVE Online Dogma Attribute, Effect VM, Fitting, Heat, Booster, Dbuff, & Abyssal Engine.
//!
//! Implements:
//! - Exact operator precedence (`-1..=9`), attribute-aware stacking penalty curve, and passive recharge
//! - 75-opcode Dogma Expression Tree VM (`DogmaOpcode`) matching `dogma_effects.json`
//! - Ship Fitting Engine (CPU, Powergrid, Rig Calibration, Slot & Hardpoint validation, Module States)
//! - Thermodynamics & Rack Heat Propagation (`#29`–`#34`)
//! - Combat Booster Side-Effect Probability & Penalty Rolling (`#35`–`#36`)
//! - Command Burst `dbuff` Aggregation (`#37`)
//! - Abyssal Mutaplasmid Dynamic Item Attribute Rolling (`#38`)

#![forbid(unsafe_code)]

pub use eve_sde::{attr_ids, EffectModifierSpec};
use eve_sde::{AttributeId, DogmaEffectType, EffectId, SdeRegistry, TypeId};
use serde::{Deserialize, Serialize};

/// EVE Dogma Modifier Operator order (executed strictly in ascending order).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ModifierOperator {
    /// Overrides the base value before multipliers (`dgmAssPreAssignment`, SDE op `-1`).
    PreAssign = 0,
    /// Multiplies the base value directly (`dgmAssPreMul`, SDE op `0`).
    PreMul = 1,
    /// Divides the base value directly (`dgmAssPreDiv`, SDE op `1`).
    PreDiv = 2,
    /// Adds a flat value (`dgmAssModAdd`, SDE op `2`).
    ModAdd = 3,
    /// Subtracts a flat value (`dgmAssModSub`, SDE op `3`).
    ModSub = 4,
    /// Multiplies after flat additions (`dgmAssPostMul`, SDE op `4`).
    PostMul = 5,
    /// Divides after flat additions (`dgmAssPostDiv`, SDE op `5`).
    PostDiv = 6,
    /// Applies a percentage bonus/penalty (`dgmAssPostPercent`, SDE op `6`).
    PostPercent = 7,
    /// Applies a stacking-penalty-exempt percentage bonus/penalty (`dgmAssPostPercentUnNerfed`, SDE op `8`).
    PostPercentUnNerfed = 8,
    /// In the V24.01 SDE, operator `9` (`dgmAssSpecialSkillRelationship`) only appears on `effect_id = 132`
    /// (`skillEffect`, `modified_attribute_id: 280` `skillLevel`, `modifying_attribute_id: 275` `skillTimeConstant`)
    /// where `SkillLevelAttribute.AddIncomingModifier` is a no-op and `9` is not in CCP's `dgmOperatorPrecedence` (`[-1, 0, 1, 2, 3, 4, 5, 6, 8, 7]`).
    SpecialSkillRelationship = 9,
    /// Final hard override (`dgmAssPostAssignment`, SDE op `7`).
    PostAssign = 10,
}

impl ModifierOperator {
    /// Converts an EVE SDE `operation` integer (`-1..=9`) into a [`ModifierOperator`].
    pub fn try_from_sde_op(op: i32) -> Option<Self> {
        match op {
            -1 => Some(Self::PreAssign),
            0 => Some(Self::PreMul),
            1 => Some(Self::PreDiv),
            2 => Some(Self::ModAdd),
            3 => Some(Self::ModSub),
            4 => Some(Self::PostMul),
            5 => Some(Self::PostDiv),
            6 => Some(Self::PostPercent),
            7 => Some(Self::PostAssign),
            8 => Some(Self::PostPercentUnNerfed),
            9 => Some(Self::SpecialSkillRelationship),
            _ => None,
        }
    }
}

/// Authoritative 75-operand Dogma Expression VM opcodes (`docs/index/dogma_effects.json`).
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DogmaVmOpcode {
    Add = 1,
    AddGangGrpMod = 2,
    AddGangItmMod = 3,
    AddGangOwnSrqMod = 4,
    AddGangSrqMod = 5,
    AddItmMod = 6,
    AddLocGrpMod = 7,
    AddLocMod = 8,
    AddLocSrqMod = 9,
    AddOwnSrqMod = 10,
    And = 11,
    Att = 12,
    Attack = 13,
    CargoScan = 14,
    CheatTeleDock = 15,
    CheatTeleGate = 16,
    Combine = 17,
    Decelo = 18,
    DefAssociation = 19,
    DefAttribute = 20,
    DefBool = 21,
    DefEnvid = 22,
    DefFloat = 23,
    DefGroup = 24,
    DefInt = 25,
    DefOperator = 26,
    DefString = 27,
    DefType = 28,
    ECM = 29,
    Efct = 30,
    Eff = 31,
    AreaOfEffect = 32,
    Eq = 33,
    Eval = 34,
    GetAttr = 35,
    GetType = 36,
    Gm = 37,
    Gt = 38,
    Gte = 39,
    If = 40,
    Inc = 41,
    Launch = 42,
    LaunchDefender = 43,
    LaunchDrone = 44,
    LaunchFoF = 45,
    Mine = 46,
    Minus = 47,
    Mul = 48,
    Or = 49,
    PowerBoost = 50,
    Rd = 51,
    RmGangGrpMod = 52,
    RmGangItmMod = 53,
    RmGangOwnSrqMod = 54,
    RmGangSrqMod = 55,
    RmItmMod = 56,
    RmLocGrpMod = 57,
    RmLocMod = 58,
    RmLocSrqMod = 59,
    RmOwnSrqMod = 60,
    Rsa = 61,
    Set = 62,
    ShipScan = 63,
    SkillCheck = 64,
    Splice = 65,
    Sub = 66,
    SurveyScan = 67,
    TargetHostile = 68,
    TargetSilent = 69,
    ToolTargetSkills = 70,
    UserError = 71,
    VerifyTargetGroup = 72,
    Coord = 73,
    Dec = 74,
    TransferCap = 75,
}

impl DogmaVmOpcode {
    pub fn from_u8(v: u8) -> Option<Self> {
        if (1..=75).contains(&v) {
            let table = [
                Self::Add,
                Self::AddGangGrpMod,
                Self::AddGangItmMod,
                Self::AddGangOwnSrqMod,
                Self::AddGangSrqMod,
                Self::AddItmMod,
                Self::AddLocGrpMod,
                Self::AddLocMod,
                Self::AddLocSrqMod,
                Self::AddOwnSrqMod,
                Self::And,
                Self::Att,
                Self::Attack,
                Self::CargoScan,
                Self::CheatTeleDock,
                Self::CheatTeleGate,
                Self::Combine,
                Self::Decelo,
                Self::DefAssociation,
                Self::DefAttribute,
                Self::DefBool,
                Self::DefEnvid,
                Self::DefFloat,
                Self::DefGroup,
                Self::DefInt,
                Self::DefOperator,
                Self::DefString,
                Self::DefType,
                Self::ECM,
                Self::Efct,
                Self::Eff,
                Self::AreaOfEffect,
                Self::Eq,
                Self::Eval,
                Self::GetAttr,
                Self::GetType,
                Self::Gm,
                Self::Gt,
                Self::Gte,
                Self::If,
                Self::Inc,
                Self::Launch,
                Self::LaunchDefender,
                Self::LaunchDrone,
                Self::LaunchFoF,
                Self::Mine,
                Self::Minus,
                Self::Mul,
                Self::Or,
                Self::PowerBoost,
                Self::Rd,
                Self::RmGangGrpMod,
                Self::RmGangItmMod,
                Self::RmGangOwnSrqMod,
                Self::RmGangSrqMod,
                Self::RmItmMod,
                Self::RmLocGrpMod,
                Self::RmLocMod,
                Self::RmLocSrqMod,
                Self::RmOwnSrqMod,
                Self::Rsa,
                Self::Set,
                Self::ShipScan,
                Self::SkillCheck,
                Self::Splice,
                Self::Sub,
                Self::SurveyScan,
                Self::TargetHostile,
                Self::TargetSilent,
                Self::ToolTargetSkills,
                Self::UserError,
                Self::VerifyTargetGroup,
                Self::Coord,
                Self::Dec,
                Self::TransferCap,
            ];
            Some(table[(v - 1) as usize])
        } else {
            None
        }
    }
}

/// AST node for evaluating a Dogma expression tree (`dgmExpressions`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DogmaExpressionNode {
    pub opcode: DogmaVmOpcode,
    pub value_f64: Option<f64>,
    pub attribute_id: Option<AttributeId>,
    pub operator: Option<ModifierOperator>,
    pub arg1: Option<Box<DogmaExpressionNode>>,
    pub arg2: Option<Box<DogmaExpressionNode>>,
}

impl DogmaExpressionNode {
    /// Evaluates a Dogma expression node against a ship/module attribute state.
    pub fn evaluate(&self, item: &DogmaItemState, sde: &SdeRegistry) -> f64 {
        match self.opcode {
            DogmaVmOpcode::DefInt
            | DogmaVmOpcode::DefFloat
            | DogmaVmOpcode::DefBool
            | DogmaVmOpcode::DefType
            | DogmaVmOpcode::DefGroup
            | DogmaVmOpcode::DefAssociation
            | DogmaVmOpcode::DefOperator
            | DogmaVmOpcode::DefEnvid
            | DogmaVmOpcode::DefString => self.value_f64.unwrap_or(0.0),
            DogmaVmOpcode::GetType => item.type_id as f64,
            DogmaVmOpcode::DefAttribute | DogmaVmOpcode::GetAttr | DogmaVmOpcode::Att => self
                .attribute_id
                .map(|attr_id| item.get_attribute(attr_id, sde))
                .unwrap_or_else(|| {
                    self.arg2
                        .as_ref()
                        .and_then(|n| n.attribute_id)
                        .map(|attr_id| item.get_attribute(attr_id, sde))
                        .unwrap_or(0.0)
                }),
            DogmaVmOpcode::Add | DogmaVmOpcode::Inc => {
                let a = self.arg1.as_ref().map(|n| n.evaluate(item, sde)).unwrap_or(0.0);
                let b = self.arg2.as_ref().map(|n| n.evaluate(item, sde)).unwrap_or(1.0);
                a + b
            }
            DogmaVmOpcode::Sub | DogmaVmOpcode::Minus | DogmaVmOpcode::Dec => {
                let a = self.arg1.as_ref().map(|n| n.evaluate(item, sde)).unwrap_or(0.0);
                let b = self.arg2.as_ref().map(|n| n.evaluate(item, sde)).unwrap_or(1.0);
                a - b
            }
            DogmaVmOpcode::Mul => {
                let a = self.arg1.as_ref().map(|n| n.evaluate(item, sde)).unwrap_or(1.0);
                let b = self.arg2.as_ref().map(|n| n.evaluate(item, sde)).unwrap_or(1.0);
                a * b
            }
            DogmaVmOpcode::And => {
                let a = self.arg1.as_ref().map(|n| n.evaluate(item, sde)).unwrap_or(0.0) != 0.0;
                let b = self.arg2.as_ref().map(|n| n.evaluate(item, sde)).unwrap_or(0.0) != 0.0;
                if a && b { 1.0 } else { 0.0 }
            }
            DogmaVmOpcode::Or => {
                let a = self.arg1.as_ref().map(|n| n.evaluate(item, sde)).unwrap_or(0.0) != 0.0;
                let b = self.arg2.as_ref().map(|n| n.evaluate(item, sde)).unwrap_or(0.0) != 0.0;
                if a || b { 1.0 } else { 0.0 }
            }
            DogmaVmOpcode::Gt => {
                let a = self.arg1.as_ref().map(|n| n.evaluate(item, sde)).unwrap_or(0.0);
                let b = self.arg2.as_ref().map(|n| n.evaluate(item, sde)).unwrap_or(0.0);
                if a > b { 1.0 } else { 0.0 }
            }
            DogmaVmOpcode::Gte => {
                let a = self.arg1.as_ref().map(|n| n.evaluate(item, sde)).unwrap_or(0.0);
                let b = self.arg2.as_ref().map(|n| n.evaluate(item, sde)).unwrap_or(0.0);
                if a >= b { 1.0 } else { 0.0 }
            }
            DogmaVmOpcode::Eq => {
                let a = self.arg1.as_ref().map(|n| n.evaluate(item, sde)).unwrap_or(0.0);
                let b = self.arg2.as_ref().map(|n| n.evaluate(item, sde)).unwrap_or(0.0);
                if (a - b).abs() < 1e-9 { 1.0 } else { 0.0 }
            }
            DogmaVmOpcode::If => {
                let cond = self.arg1.as_ref().map(|n| n.evaluate(item, sde)).unwrap_or(0.0);
                if cond != 0.0 {
                    self.arg2.as_ref().map(|n| n.evaluate(item, sde)).unwrap_or(1.0)
                } else {
                    0.0
                }
            }
            DogmaVmOpcode::Eval | DogmaVmOpcode::Eff | DogmaVmOpcode::Efct | DogmaVmOpcode::Set => {
                self.arg2
                    .as_ref()
                    .or(self.arg1.as_ref())
                    .map(|n| n.evaluate(item, sde))
                    .unwrap_or_else(|| self.value_f64.unwrap_or(1.0))
            }
            DogmaVmOpcode::Combine | DogmaVmOpcode::Splice => {
                let _ = self.arg1.as_ref().map(|n| n.evaluate(item, sde));
                self.arg2.as_ref().map(|n| n.evaluate(item, sde)).unwrap_or(1.0)
            }
            DogmaVmOpcode::SkillCheck => {
                let skill_id = self
                    .arg1
                    .as_ref()
                    .map(|n| n.evaluate(item, sde))
                    .or(self.value_f64)
                    .unwrap_or(0.0) as i64;
                let req_level = self
                    .arg2
                    .as_ref()
                    .map(|n| n.evaluate(item, sde))
                    .unwrap_or(1.0) as u8;
                if skill_id > 0 {
                    let trained = item
                        .skill_levels
                        .get(&skill_id)
                        .copied()
                        .unwrap_or_else(|| {
                            if item.type_id as i64 == skill_id {
                                item.get_attribute(280, sde) as u8
                            } else {
                                0
                            }
                        });
                    if trained >= req_level.max(1) {
                        1.0
                    } else {
                        0.0
                    }
                } else {
                    let type_info = sde.types.get(&item.type_id);
                    let mut all_met = true;
                    for i in 0..6 {
                        let s_attr = REQUIRED_SKILL_ATTR_IDS[i];
                        let l_attr = REQUIRED_SKILL_LEVEL_ATTR_IDS[i];
                        let req_s = item
                            .base_attributes
                            .get(&s_attr)
                            .copied()
                            .or_else(|| type_info.and_then(|t| t.default_attributes.get(&s_attr).copied()))
                            .unwrap_or(0.0) as i64;
                        let req_l = item
                            .base_attributes
                            .get(&l_attr)
                            .copied()
                            .or_else(|| type_info.and_then(|t| t.default_attributes.get(&l_attr).copied()))
                            .unwrap_or(1.0) as u8;
                        if req_s > 0 {
                            let trained = item.skill_levels.get(&req_s).copied().unwrap_or(0);
                            if trained < req_l.max(1) {
                                all_met = false;
                                break;
                            }
                        }
                    }
                    if all_met {
                        1.0
                    } else {
                        0.0
                    }
                }
            }
            DogmaVmOpcode::ToolTargetSkills => {
                let target_type = self
                    .arg1
                    .as_ref()
                    .map(|n| n.evaluate(item, sde))
                    .or(self.value_f64)
                    .map(|v| v as TypeId)
                    .unwrap_or(item.type_id);
                let mut all_met = true;
                let type_info = sde.types.get(&target_type);
                for i in 0..6 {
                    let s_attr = REQUIRED_SKILL_ATTR_IDS[i];
                    let l_attr = REQUIRED_SKILL_LEVEL_ATTR_IDS[i];
                    let req_s = item
                        .base_attributes
                        .get(&s_attr)
                        .copied()
                        .or_else(|| type_info.and_then(|t| t.default_attributes.get(&s_attr).copied()))
                        .unwrap_or(0.0) as i64;
                    let req_l = item
                        .base_attributes
                        .get(&l_attr)
                        .copied()
                        .or_else(|| type_info.and_then(|t| t.default_attributes.get(&l_attr).copied()))
                        .unwrap_or(1.0) as u8;
                    if req_s > 0 {
                        let trained = item.skill_levels.get(&req_s).copied().unwrap_or(0);
                        if trained < req_l.max(1) {
                            all_met = false;
                            break;
                        }
                    }
                }
                if all_met {
                    1.0
                } else {
                    0.0
                }
            }
            DogmaVmOpcode::VerifyTargetGroup => {
                let req_grp = self
                    .arg1
                    .as_ref()
                    .map(|n| n.evaluate(item, sde))
                    .or_else(|| self.arg2.as_ref().map(|n| n.evaluate(item, sde)))
                    .or(self.value_f64)
                    .unwrap_or(0.0) as u32;
                let actual_grp = item
                    .group_id
                    .or_else(|| sde.types.get(&item.type_id).map(|t| t.group_id))
                    .unwrap_or(0);
                if req_grp > 0 {
                    if actual_grp == req_grp {
                        1.0
                    } else {
                        0.0
                    }
                } else {
                    1.0
                }
            }
            DogmaVmOpcode::UserError => 0.0,
            _ => self.value_f64.unwrap_or(1.0),
        }
    }

    /// Executes a modifier-applying or modifier-removing Dogma VM expression tree against `target`.
    pub fn execute_on_target(
        &self,
        source_item_id: i64,
        source: &DogmaItemState,
        target: &mut DogmaItemState,
        sde: &SdeRegistry,
    ) {
        self.execute_on_target_with_effect(source_item_id, None, source, target, sde);
    }

    /// Executes a modifier-applying or modifier-removing Dogma VM expression tree against `target`,
    /// tagging added or removed modifiers with `effect_id` so multi-effect items remove individual effects cleanly.
    /// Executes a modifier-applying or modifier-removing Dogma VM expression tree against `target`,
    /// tagging added or removed modifiers with `effect_id` so multi-effect items remove individual effects cleanly.
    pub fn execute_on_target_with_effect(
        &self,
        source_item_id: i64,
        effect_id: Option<EffectId>,
        source: &DogmaItemState,
        target: &mut DogmaItemState,
        sde: &SdeRegistry,
    ) {
        self.execute_on_target_with_effect_and_scale(source_item_id, effect_id, source, target, 1.0, sde);
    }

    /// Executes a modifier-applying Dogma VM expression tree scaled by an attenuation factor (e.g. range falloff).
    pub fn execute_on_target_with_effect_and_scale(
        &self,
        source_item_id: i64,
        effect_id: Option<EffectId>,
        source: &DogmaItemState,
        target: &mut DogmaItemState,
        scale: f64,
        sde: &SdeRegistry,
    ) {
        match self.opcode {
            DogmaVmOpcode::Combine | DogmaVmOpcode::Splice => {
                if let Some(a1) = &self.arg1 {
                    a1.execute_on_target_with_effect_and_scale(source_item_id, effect_id, source, target, scale, sde);
                }
                if let Some(a2) = &self.arg2 {
                    a2.execute_on_target_with_effect_and_scale(source_item_id, effect_id, source, target, scale, sde);
                }
            }
            DogmaVmOpcode::If => {
                let cond = self.arg1.as_ref().map(|n| n.evaluate(source, sde)).unwrap_or(0.0);
                if cond != 0.0 {
                    if let Some(a2) = &self.arg2 {
                        a2.execute_on_target_with_effect_and_scale(
                            source_item_id,
                            effect_id,
                            source,
                            target,
                            scale,
                            sde,
                        );
                    }
                }
            }
            DogmaVmOpcode::AddItmMod
            | DogmaVmOpcode::AddLocMod
            | DogmaVmOpcode::AddLocGrpMod
            | DogmaVmOpcode::AddLocSrqMod
            | DogmaVmOpcode::AddOwnSrqMod
            | DogmaVmOpcode::AddGangGrpMod
            | DogmaVmOpcode::AddGangItmMod
            | DogmaVmOpcode::AddGangOwnSrqMod
            | DogmaVmOpcode::AddGangSrqMod => {
                // Enforce strict group filter when specified in arg1 (unknown/0 group never matches)
                if matches!(
                    self.opcode,
                    DogmaVmOpcode::AddLocGrpMod | DogmaVmOpcode::AddGangGrpMod
                ) {
                    if let Some(filter_node) = &self.arg1 {
                        if filter_node.opcode == DogmaVmOpcode::DefGroup {
                            if let Some(req_grp) = filter_node.value_f64 {
                                let actual_grp = target
                                    .group_id
                                    .or_else(|| sde.types.get(&target.type_id).map(|t| t.group_id))
                                    .unwrap_or(0);
                                if actual_grp == 0 || actual_grp != req_grp as u32 {
                                    return;
                                }
                            }
                        }
                    }
                }
                // Enforce strict skill-requirement filter when specified in arg1 (items without the required skill never match)
                if matches!(
                    self.opcode,
                    DogmaVmOpcode::AddLocSrqMod
                        | DogmaVmOpcode::AddOwnSrqMod
                        | DogmaVmOpcode::AddGangSrqMod
                        | DogmaVmOpcode::AddGangOwnSrqMod
                ) {
                    if let Some(filter_node) = &self.arg1 {
                        if filter_node.opcode == DogmaVmOpcode::DefType {
                            if let Some(req_skill) = filter_node.value_f64 {
                                let req_skill_tid = req_skill as TypeId;
                                if !item_requires_skill(target, req_skill_tid, sde) {
                                    return;
                                }
                            }
                        }
                    }
                }

                let op = self.operator.unwrap_or(ModifierOperator::PostPercent);
                let target_attr = self
                    .attribute_id
                    .or_else(|| self.arg1.as_ref().and_then(|n| n.attribute_id))
                    .unwrap_or(attr_ids::MAX_VELOCITY);
                let raw_base_val = self
                    .arg2
                    .as_ref()
                    .map(|n| n.evaluate(source, sde))
                    .or(self.value_f64)
                    .unwrap_or(0.0);
                // Polarized weapons (`feat:modules.polarized-weapons-zero-resists`) author `1978` (`resistanceKiller` = 100.0)
                // on effect `5995` (`resistanceKillerShieldArmorAll`, `PostAssign`) targeting shield & armor resonance `267..=274`,
                // where `1.0` represents 100% damage taken (0% resistance), matching hull resonance `109..=113` (`5994`, `1979` = 1.0).
                let base_val = if effect_id == Some(5995)
                    && op == ModifierOperator::PostAssign
                    && (267..=274).contains(&target_attr)
                    && (raw_base_val - 100.0).abs() < 1e-6
                {
                    1.0
                } else {
                    raw_base_val
                };
                let val = if (scale - 1.0).abs() < 1e-9 {
                    base_val
                } else {
                    match op {
                        ModifierOperator::PreMul | ModifierOperator::PostMul => {
                            1.0 + (base_val - 1.0) * scale
                        }
                        ModifierOperator::PreDiv | ModifierOperator::PostDiv => {
                            if base_val != 0.0 {
                                let factor = 1.0 + ((1.0 / base_val) - 1.0) * scale;
                                if factor != 0.0 {
                                    1.0 / factor
                                } else {
                                    base_val
                                }
                            } else {
                                base_val
                            }
                        }
                        ModifierOperator::PostPercent
                        | ModifierOperator::PostPercentUnNerfed
                        | ModifierOperator::ModAdd
                        | ModifierOperator::ModSub => base_val * scale,
                        _ => base_val,
                    }
                };
                let op_penalized = !matches!(
                    op,
                    ModifierOperator::ModAdd
                        | ModifierOperator::ModSub
                        | ModifierOperator::PreAssign
                        | ModifierOperator::PostAssign
                        | ModifierOperator::PostPercentUnNerfed
                        | ModifierOperator::SpecialSkillRelationship
                );
                let attr_non_stackable = sde
                    .attributes
                    .get(&target_attr)
                    .map(|a| !a.stackable)
                    .unwrap_or(false);
                let stacking_penalized =
                    op_penalized && attr_non_stackable && !source_is_stacking_exempt(source, sde);
                target.add_modifier(AttributeModifier {
                    source_item_id,
                    effect_id,
                    target_attribute_id: target_attr,
                    operator: op,
                    value: val,
                    stacking_penalized,
                });
            }
            DogmaVmOpcode::RmItmMod
            | DogmaVmOpcode::RmLocMod
            | DogmaVmOpcode::RmLocGrpMod
            | DogmaVmOpcode::RmLocSrqMod
            | DogmaVmOpcode::RmOwnSrqMod
            | DogmaVmOpcode::RmGangGrpMod
            | DogmaVmOpcode::RmGangItmMod
            | DogmaVmOpcode::RmGangOwnSrqMod
            | DogmaVmOpcode::RmGangSrqMod => {
                target.remove_modifiers_from_source_effect(source_item_id, effect_id);
            }
            _ => {}
        }
    }
}

/// All 6 SDE skill-requirement attribute IDs (`requiredSkill1`..`requiredSkill6`).
pub const REQUIRED_SKILL_ATTR_IDS: [AttributeId; 6] = [182, 183, 184, 1285, 1289, 1290];

/// All 6 SDE skill-requirement level attribute IDs (`requiredSkill1Level`..`requiredSkill6Level`).
pub const REQUIRED_SKILL_LEVEL_ATTR_IDS: [AttributeId; 6] = [277, 278, 279, 1286, 1287, 1288];

/// Returns true if `source` belongs to a category/group exempt from EVE Dogma stacking penalties:
/// Skills (`category_id == 16`), Ships (`category_id == 6`), Charges (`category_id == 8`),
/// Implants/Boosters (`category_id == 20` or `group_id == 303`), and Subsystems (`category_id == 32`).
pub fn source_is_stacking_exempt(source: &DogmaItemState, sde: &SdeRegistry) -> bool {
    let cat_id = source
        .category_id
        .or_else(|| sde.types.get(&source.type_id).map(|t| t.category_id));
    let grp_id = source
        .group_id
        .or_else(|| sde.types.get(&source.type_id).map(|t| t.group_id));
    matches!(cat_id, Some(6 | 8 | 16 | 20 | 32)) || matches!(grp_id, Some(303))
}

/// Returns true if `item` defines any `requiredSkill1..6` in its base or SDE attributes.
pub fn item_has_any_required_skill(item: &DogmaItemState, sde: &SdeRegistry) -> bool {
    for &attr_id in &REQUIRED_SKILL_ATTR_IDS {
        if item.base_attributes.get(&attr_id).copied().unwrap_or(0.0) > 0.0 {
            return true;
        }
    }
    if let Some(t) = sde.types.get(&item.type_id) {
        for &attr_id in &REQUIRED_SKILL_ATTR_IDS {
            if t.default_attributes.get(&attr_id).copied().unwrap_or(0.0) > 0.0 {
                return true;
            }
        }
    }
    false
}

/// Returns true if `item` directly requires `skill_type_id` via `requiredSkill1..6`
/// in `base_attributes` or `sde.types`.
pub fn item_requires_skill(item: &DogmaItemState, skill_type_id: TypeId, sde: &SdeRegistry) -> bool {
    for &attr_id in &REQUIRED_SKILL_ATTR_IDS {
        if let Some(&v) = item.base_attributes.get(&attr_id) {
            if v > 0.0 && (v.round() as TypeId) == skill_type_id {
                return true;
            }
        }
    }
    if let Some(t) = sde.types.get(&item.type_id) {
        for &attr_id in &REQUIRED_SKILL_ATTR_IDS {
            if let Some(&v) = t.default_attributes.get(&attr_id) {
                if v > 0.0 && (v.round() as TypeId) == skill_type_id {
                    return true;
                }
            }
        }
    }
    false
}

/// Authoritative V24 Dogma `modifierInfo` target domain (`itemID`, `shipID`, `charID`, `targetID`/`target`, `otherID`, `structureID`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DogmaDomain {
    Item,
    Ship,
    Char,
    Target,
    Other,
    Structure,
}

impl DogmaDomain {
    pub fn from_sde_str(s: &str) -> Option<Self> {
        match s {
            "itemID" => Some(Self::Item),
            "shipID" => Some(Self::Ship),
            "charID" => Some(Self::Char),
            "targetID" | "target" => Some(Self::Target),
            "otherID" => Some(Self::Other),
            "structureID" => Some(Self::Structure),
            _ => None,
        }
    }
}

/// Authoritative V24 Dogma `modifierInfo` function (`ItemModifier`, `ItemModifierWithFalloff`, `LocationModifier`,
/// `LocationGroupModifier`, `LocationRequiredSkillModifier`, `OwnerRequiredSkillModifier`, `EffectStopper`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DogmaModifierFunc {
    ItemModifier,
    ItemModifierWithFalloff,
    LocationModifier,
    LocationGroupModifier,
    LocationRequiredSkillModifier,
    OwnerRequiredSkillModifier,
    EffectStopper,
}

impl DogmaModifierFunc {
    pub fn from_sde_str(s: &str) -> Option<Self> {
        match s {
            "ItemModifier" => Some(Self::ItemModifier),
            "ItemModifierWithFalloff" => Some(Self::ItemModifierWithFalloff),
            "LocationModifier" => Some(Self::LocationModifier),
            "LocationGroupModifier" => Some(Self::LocationGroupModifier),
            "LocationRequiredSkillModifier" => Some(Self::LocationRequiredSkillModifier),
            "OwnerRequiredSkillModifier" => Some(Self::OwnerRequiredSkillModifier),
            "EffectStopper" => Some(Self::EffectStopper),
            _ => None,
        }
    }
}

/// Compiles a single SDE [`EffectModifierSpec`] (`modifierInfo` entry) into a [`DogmaExpressionNode`]
/// using the 75-opcode [`DogmaVmOpcode`] instruction set.
pub fn compile_modifier_spec_to_vm(
    spec: &EffectModifierSpec,
    is_remove: bool,
) -> Option<DogmaExpressionNode> {
    let func = DogmaModifierFunc::from_sde_str(&spec.func)?;
    if func == DogmaModifierFunc::EffectStopper {
        let stopped_eid = spec.effect_id?;
        return Some(DogmaExpressionNode {
            opcode: DogmaVmOpcode::Efct,
            value_f64: Some(stopped_eid as f64),
            attribute_id: None,
            operator: None,
            arg1: None,
            arg2: None,
        });
    }

    let op = ModifierOperator::try_from_sde_op(spec.operator)?;
    let opcode = match (func, is_remove) {
        (DogmaModifierFunc::ItemModifier | DogmaModifierFunc::ItemModifierWithFalloff, false) => {
            DogmaVmOpcode::AddItmMod
        }
        (DogmaModifierFunc::ItemModifier | DogmaModifierFunc::ItemModifierWithFalloff, true) => {
            DogmaVmOpcode::RmItmMod
        }
        (DogmaModifierFunc::LocationModifier, false) => DogmaVmOpcode::AddLocMod,
        (DogmaModifierFunc::LocationModifier, true) => DogmaVmOpcode::RmLocMod,
        (DogmaModifierFunc::LocationGroupModifier, false) => DogmaVmOpcode::AddLocGrpMod,
        (DogmaModifierFunc::LocationGroupModifier, true) => DogmaVmOpcode::RmLocGrpMod,
        (DogmaModifierFunc::LocationRequiredSkillModifier, false) => DogmaVmOpcode::AddLocSrqMod,
        (DogmaModifierFunc::LocationRequiredSkillModifier, true) => DogmaVmOpcode::RmLocSrqMod,
        (DogmaModifierFunc::OwnerRequiredSkillModifier, false) => DogmaVmOpcode::AddOwnSrqMod,
        (DogmaModifierFunc::OwnerRequiredSkillModifier, true) => DogmaVmOpcode::RmOwnSrqMod,
        (DogmaModifierFunc::EffectStopper, _) => unreachable!(),
    };

    let arg1 = match func {
        DogmaModifierFunc::LocationGroupModifier => spec.group_id.map(|gid| {
            Box::new(DogmaExpressionNode {
                opcode: DogmaVmOpcode::DefGroup,
                value_f64: Some(gid as f64),
                attribute_id: Some(spec.modified_attribute_id),
                operator: None,
                arg1: None,
                arg2: None,
            })
        }),
        DogmaModifierFunc::LocationRequiredSkillModifier
        | DogmaModifierFunc::OwnerRequiredSkillModifier => spec.skill_type_id.map(|stid| {
            Box::new(DogmaExpressionNode {
                opcode: DogmaVmOpcode::DefType,
                value_f64: Some(stid as f64),
                attribute_id: Some(spec.modified_attribute_id),
                operator: None,
                arg1: None,
                arg2: None,
            })
        }),
        _ => None,
    };

    let arg2 = Some(Box::new(DogmaExpressionNode {
        opcode: DogmaVmOpcode::GetAttr,
        value_f64: None,
        attribute_id: Some(spec.modifying_attribute_id),
        operator: None,
        arg1: None,
        arg2: None,
    }));

    Some(DogmaExpressionNode {
        opcode,
        value_f64: None,
        attribute_id: Some(spec.modified_attribute_id),
        operator: Some(op),
        arg1,
        arg2,
    })
}

/// Compiles all [`EffectModifierSpec`] entries on an [`DogmaEffectType`] into a combined [`DogmaExpressionNode`] tree.
pub fn compile_effect_to_vm(effect: &DogmaEffectType, is_remove: bool) -> Option<DogmaExpressionNode> {
    let mut tree: Option<DogmaExpressionNode> = None;
    for spec in &effect.modifiers {
        let node = compile_modifier_spec_to_vm(spec, is_remove)?;
        tree = Some(match tree {
            None => node,
            Some(prev) => DogmaExpressionNode {
                opcode: DogmaVmOpcode::Combine,
                value_f64: None,
                attribute_id: None,
                operator: None,
                arg1: Some(Box::new(prev)),
                arg2: Some(Box::new(node)),
            },
        });
    }
    tree
}

mod combat_eval;
mod context;
mod evaluation;
mod fitting;
#[cfg(test)]
mod tests;

pub use combat_eval::*;
pub use context::*;
pub use evaluation::*;
pub use fitting::*;
