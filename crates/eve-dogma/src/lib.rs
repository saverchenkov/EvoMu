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

use eve_sde::{
    attr_ids, AttributeId, CategoryId, DogmaEffectType, EffectId, EffectModifierSpec, GroupId,
    SdeRegistry, TypeId,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

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
    /// Applies a skill-relationship percentage modifier (`dgmAssSpecialSkillRelationship`, SDE op `9`).
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
            DogmaVmOpcode::SkillCheck | DogmaVmOpcode::VerifyTargetGroup | DogmaVmOpcode::ToolTargetSkills => {
                1.0
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
    pub fn execute_on_target_with_effect(
        &self,
        source_item_id: i64,
        effect_id: Option<EffectId>,
        source: &DogmaItemState,
        target: &mut DogmaItemState,
        sde: &SdeRegistry,
    ) {
        match self.opcode {
            DogmaVmOpcode::Combine | DogmaVmOpcode::Splice => {
                if let Some(a1) = &self.arg1 {
                    a1.execute_on_target_with_effect(source_item_id, effect_id, source, target, sde);
                }
                if let Some(a2) = &self.arg2 {
                    a2.execute_on_target_with_effect(source_item_id, effect_id, source, target, sde);
                }
            }
            DogmaVmOpcode::If => {
                let cond = self.arg1.as_ref().map(|n| n.evaluate(source, sde)).unwrap_or(0.0);
                if cond != 0.0 {
                    if let Some(a2) = &self.arg2 {
                        a2.execute_on_target_with_effect(
                            source_item_id,
                            effect_id,
                            source,
                            target,
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
                let val = self
                    .arg2
                    .as_ref()
                    .map(|n| n.evaluate(source, sde))
                    .or(self.value_f64)
                    .unwrap_or(0.0);
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

/// Authoritative V24 Dogma `modifierInfo` function (`ItemModifier`, `LocationModifier`,
/// `LocationGroupModifier`, `LocationRequiredSkillModifier`, `OwnerRequiredSkillModifier`, `EffectStopper`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DogmaModifierFunc {
    ItemModifier,
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
        (DogmaModifierFunc::ItemModifier, false) => DogmaVmOpcode::AddItmMod,
        (DogmaModifierFunc::ItemModifier, true) => DogmaVmOpcode::RmItmMod,
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

/// Multi-domain evaluation context for executing V24 Dogma `modifierInfo` effects.
///
/// Supports all 6 domains (`itemID`, `shipID`, `charID`, `targetID`/`target`, `otherID`, `structureID`)
/// with isolated per-domain location item lists and all 6 modifier functions (`ItemModifier`,
/// `LocationModifier`, `LocationGroupModifier`, `LocationRequiredSkillModifier`,
/// `OwnerRequiredSkillModifier`, `EffectStopper`).
#[derive(Debug, Clone)]
pub struct DogmaEffectContext {
    pub item: DogmaItemState,
    pub ship: DogmaItemState,
    pub character: DogmaItemState,
    pub target: DogmaItemState,
    pub other: DogmaItemState,
    pub structure: DogmaItemState,
    /// Items located inside `shipID` (fitted modules, rigs, subsystems, charges in ship).
    pub ship_located_items: Vec<DogmaItemState>,
    /// Items located inside `structureID` (fitted structure modules/rigs/service modules).
    pub structure_located_items: Vec<DogmaItemState>,
    /// Items located on `charID` (implants, boosters, skills).
    pub char_located_items: Vec<DogmaItemState>,
    /// Items located inside `targetID` / `target`.
    pub target_located_items: Vec<DogmaItemState>,
    /// Items located inside `otherID`.
    pub other_located_items: Vec<DogmaItemState>,
    /// Items located inside `itemID` (e.g. loaded charge/script inside a module).
    pub item_located_items: Vec<DogmaItemState>,
    /// Items owned by `charID` (charges in space, drones, fighters, missiles) targeted by `OwnerRequiredSkillModifier`.
    pub owned_items: Vec<DogmaItemState>,
    /// Target effect IDs stopped via `EffectStopper` (e.g. Warp Scrambler stopping MWD `6441` / MJD `6442`).
    pub stopped_target_effects: Vec<EffectId>,
}

impl DogmaEffectContext {
    pub fn new(
        item: DogmaItemState,
        ship: DogmaItemState,
        character: DogmaItemState,
        target: DogmaItemState,
        other: DogmaItemState,
        structure: DogmaItemState,
    ) -> Self {
        Self {
            item,
            ship,
            character,
            target,
            other,
            structure,
            ship_located_items: Vec::new(),
            structure_located_items: Vec::new(),
            char_located_items: Vec::new(),
            target_located_items: Vec::new(),
            other_located_items: Vec::new(),
            item_located_items: Vec::new(),
            owned_items: Vec::new(),
            stopped_target_effects: Vec::new(),
        }
    }

    pub fn entity_for_domain(&self, domain: DogmaDomain) -> &DogmaItemState {
        match domain {
            DogmaDomain::Item => &self.item,
            DogmaDomain::Ship => &self.ship,
            DogmaDomain::Char => &self.character,
            DogmaDomain::Target => &self.target,
            DogmaDomain::Other => &self.other,
            DogmaDomain::Structure => &self.structure,
        }
    }

    pub fn entity_for_domain_mut(&mut self, domain: DogmaDomain) -> &mut DogmaItemState {
        match domain {
            DogmaDomain::Item => &mut self.item,
            DogmaDomain::Ship => &mut self.ship,
            DogmaDomain::Char => &mut self.character,
            DogmaDomain::Target => &mut self.target,
            DogmaDomain::Other => &mut self.other,
            DogmaDomain::Structure => &mut self.structure,
        }
    }

    pub fn located_items_for_domain(&self, domain: DogmaDomain) -> &[DogmaItemState] {
        match domain {
            DogmaDomain::Item => &self.item_located_items,
            DogmaDomain::Ship => &self.ship_located_items,
            DogmaDomain::Char => &self.char_located_items,
            DogmaDomain::Target => &self.target_located_items,
            DogmaDomain::Other => &self.other_located_items,
            DogmaDomain::Structure => &self.structure_located_items,
        }
    }

    pub fn located_items_for_domain_mut(
        &mut self,
        domain: DogmaDomain,
    ) -> &mut Vec<DogmaItemState> {
        match domain {
            DogmaDomain::Item => &mut self.item_located_items,
            DogmaDomain::Ship => &mut self.ship_located_items,
            DogmaDomain::Char => &mut self.char_located_items,
            DogmaDomain::Target => &mut self.target_located_items,
            DogmaDomain::Other => &mut self.other_located_items,
            DogmaDomain::Structure => &mut self.structure_located_items,
        }
    }

    /// Applies a single [`EffectModifierSpec`] from `self.item` onto the domain/targets in this context.
    /// Returns the number of target entities/items modified (or stopped effects recorded).
    pub fn apply_modifier_spec(
        &mut self,
        source_item_id: i64,
        spec: &EffectModifierSpec,
        sde: &SdeRegistry,
    ) -> usize {
        self.apply_modifier_spec_with_effect(source_item_id, None, spec, sde)
    }

    /// Applies a single [`EffectModifierSpec`] tagged with `effect_id`.
    pub fn apply_modifier_spec_with_effect(
        &mut self,
        source_item_id: i64,
        effect_id: Option<EffectId>,
        spec: &EffectModifierSpec,
        sde: &SdeRegistry,
    ) -> usize {
        let Some(domain) = DogmaDomain::from_sde_str(&spec.domain) else {
            return 0;
        };
        let Some(func) = DogmaModifierFunc::from_sde_str(&spec.func) else {
            return 0;
        };
        let Some(vm_node) = compile_modifier_spec_to_vm(spec, false) else {
            return 0;
        };

        if func == DogmaModifierFunc::EffectStopper {
            if let Some(eid) = spec.effect_id {
                if !self.stopped_target_effects.contains(&eid) {
                    self.stopped_target_effects.push(eid);
                }
                return 1;
            }
            return 0;
        }

        let source_snapshot = self.item.clone();
        let mut applied = 0usize;

        match func {
            DogmaModifierFunc::ItemModifier => {
                let target_entity = self.entity_for_domain_mut(domain);
                let before = target_entity.modifiers.len();
                vm_node.execute_on_target_with_effect(
                    source_item_id,
                    effect_id,
                    &source_snapshot,
                    target_entity,
                    sde,
                );
                if target_entity.modifiers.len() > before {
                    applied += 1;
                }
            }
            DogmaModifierFunc::LocationModifier
            | DogmaModifierFunc::LocationGroupModifier
            | DogmaModifierFunc::LocationRequiredSkillModifier => {
                // Location* modifiers apply strictly to items located inside `domain`, never the location entity itself.
                let list = self.located_items_for_domain_mut(domain);
                for child in list.iter_mut() {
                    let b = child.modifiers.len();
                    vm_node.execute_on_target_with_effect(
                        source_item_id,
                        effect_id,
                        &source_snapshot,
                        child,
                        sde,
                    );
                    if child.modifiers.len() > b {
                        applied += 1;
                    }
                }
            }
            DogmaModifierFunc::OwnerRequiredSkillModifier => {
                for child in self.owned_items.iter_mut() {
                    let b = child.modifiers.len();
                    vm_node.execute_on_target_with_effect(
                        source_item_id,
                        effect_id,
                        &source_snapshot,
                        child,
                        sde,
                    );
                    if child.modifiers.len() > b {
                        applied += 1;
                    }
                }
            }
            DogmaModifierFunc::EffectStopper => unreachable!(),
        }

        applied
    }

    /// Applies all [`EffectModifierSpec`] entries on `effect` from `source_item_id`, tagging each with `effect.effect_id`.
    pub fn apply_effect(
        &mut self,
        source_item_id: i64,
        effect: &DogmaEffectType,
        sde: &SdeRegistry,
    ) -> usize {
        let mut total = 0;
        for spec in &effect.modifiers {
            total += self.apply_modifier_spec_with_effect(
                source_item_id,
                Some(effect.effect_id),
                spec,
                sde,
            );
        }
        total
    }

    /// Removes only modifiers originating from `(source_item_id, effect.effect_id)` across all domains and location lists,
    /// and clears stopped target effects if `effect` had `EffectStopper` modifiers.
    pub fn remove_effect(&mut self, source_item_id: i64, effect: &DogmaEffectType) {
        let eid = Some(effect.effect_id);
        self.item
            .remove_modifiers_from_source_effect(source_item_id, eid);
        self.ship
            .remove_modifiers_from_source_effect(source_item_id, eid);
        self.character
            .remove_modifiers_from_source_effect(source_item_id, eid);
        self.target
            .remove_modifiers_from_source_effect(source_item_id, eid);
        self.other
            .remove_modifiers_from_source_effect(source_item_id, eid);
        self.structure
            .remove_modifiers_from_source_effect(source_item_id, eid);
        for c in &mut self.ship_located_items {
            c.remove_modifiers_from_source_effect(source_item_id, eid);
        }
        for c in &mut self.structure_located_items {
            c.remove_modifiers_from_source_effect(source_item_id, eid);
        }
        for c in &mut self.char_located_items {
            c.remove_modifiers_from_source_effect(source_item_id, eid);
        }
        for c in &mut self.target_located_items {
            c.remove_modifiers_from_source_effect(source_item_id, eid);
        }
        for c in &mut self.other_located_items {
            c.remove_modifiers_from_source_effect(source_item_id, eid);
        }
        for c in &mut self.item_located_items {
            c.remove_modifiers_from_source_effect(source_item_id, eid);
        }
        for c in &mut self.owned_items {
            c.remove_modifiers_from_source_effect(source_item_id, eid);
        }
        for spec in &effect.modifiers {
            if spec.func == "EffectStopper" {
                if let Some(stopped) = spec.effect_id {
                    self.stopped_target_effects.retain(|&x| x != stopped);
                }
            }
        }
    }
}

/// Represents an active modifier applied to an entity's Dogma attribute.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttributeModifier {
    pub source_item_id: i64,
    #[serde(default)]
    pub effect_id: Option<EffectId>,
    pub target_attribute_id: AttributeId,
    pub operator: ModifierOperator,
    pub value: f64,
    /// Whether the modifier source (e.g., Module/Rig/EWAR vs. Skill/Implant/Hull) is subject
    /// to stacking penalties when targeting a non-stackable attribute (`stackable == false`).
    pub stacking_penalized: bool,
}

/// Computes EVE Online's stacking penalty factor for the `n`-th (0-indexed) module modifier:
///
/// $$S(n) = e^{-(n / 2.67)^2}$$
#[inline]
pub fn stacking_penalty_factor(index: usize) -> f64 {
    let ratio = (index as f64) / 2.67;
    (-ratio * ratio).exp()
}

/// Computes the analytical EVE passive recharge curve (used by Capacitor and Shield regeneration):
///
/// $$C(t) = C_{\max} \left(1 + \left(\sqrt{\frac{C_0}{C_{\max}}} - 1\right) e^{-\frac{5 \Delta t}{\tau_{\text{recharge}}}}\right)^2$$
pub fn compute_passive_recharge(
    current: f64,
    max_capacity: f64,
    recharge_time_ms: f64,
    dt_ms: f64,
) -> f64 {
    if max_capacity <= 0.0 || recharge_time_ms <= 0.0 || current >= max_capacity {
        return current.min(max_capacity);
    }
    let c0_ratio = (current.max(0.0) / max_capacity).sqrt();
    let decay = (-5.0 * dt_ms / recharge_time_ms).exp();
    let term = 1.0 + (c0_ratio - 1.0) * decay;
    (max_capacity * term * term).clamp(0.0, max_capacity)
}

/// Simulates one time step of the EVE Online capacitor differential equation (`DG-2`):
///
/// $$\frac{dC}{dt} = \frac{10}{T} \left(\sqrt{\frac{C}{C_{\max}}} - \frac{C}{C_{\max}}\right) C_{\max}$$
pub fn simulate_capacitor(current: f64, cap_max: f64, recharge_time: f64, dt: f64) -> f64 {
    if cap_max <= 0.0 || recharge_time <= 0.0 {
        return current;
    }
    let c = current.clamp(0.0, cap_max);
    let c_ratio = (c / cap_max).clamp(0.0, 1.0);
    let c_dot = (10.0 / recharge_time) * (c_ratio.sqrt() - c_ratio) * cap_max;
    (c + c_dot * dt).clamp(0.0, cap_max)
}

/// Instantaneous capacitor recharge rate $\dot{C} = \frac{10}{T} (\sqrt{\frac{C}{C_{\max}}} - \frac{C}{C_{\max}}) C_{\max}$ (`DG-2`).
pub fn capacitor_c_dot(current: f64, cap_max: f64, recharge_time: f64) -> f64 {
    if cap_max <= 0.0 || recharge_time <= 0.0 {
        return 0.0;
    }
    let c = current.clamp(0.0, cap_max);
    let c_ratio = (c / cap_max).clamp(0.0, 1.0);
    (10.0 / recharge_time) * (c_ratio.sqrt() - c_ratio) * cap_max
}

/// Module activation state machine (`Offline`, `Online`, `Active`, `Overloaded`, `Reactivating`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ModuleState {
    Offline,
    Online,
    Active,
    Overloaded,
    Reactivating,
}

/// Fitted module slot entry inside a [`ShipFittingEngine`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FittedModule {
    pub item_id: i64,
    pub type_id: TypeId,
    pub flag_id: u16,
    pub state: ModuleState,
    pub damage_hp: f64,
    pub max_hp: f64,
    pub charge_type_id: Option<TypeId>,
    pub charge_quantity: u32,
}

/// Stateful Dogma container for a ship or item entity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DogmaItemState {
    pub item_id: i64,
    pub type_id: TypeId,
    #[serde(default)]
    pub group_id: Option<GroupId>,
    #[serde(default)]
    pub category_id: Option<CategoryId>,
    pub base_attributes: HashMap<AttributeId, f64>,
    pub modifiers: Vec<AttributeModifier>,
    /// Optional trained skill levels keyed by `source_item_id` for `SpecialSkillRelationship` (`dgmAssSpecialSkillRelationship`).
    #[serde(default)]
    pub skill_levels: HashMap<i64, u8>,
}

impl DogmaItemState {
    pub fn from_sde(item_id: i64, type_id: TypeId, sde: &SdeRegistry) -> Self {
        let mut base_attributes = HashMap::new();
        let mut group_id = None;
        let mut category_id = None;
        if let Some(t) = sde.types.get(&type_id) {
            group_id = Some(t.group_id);
            category_id = Some(t.category_id);
            base_attributes = t.default_attributes.clone();
            if t.mass > 0.0 {
                base_attributes.entry(attr_ids::MASS).or_insert(t.mass);
            }
            if t.capacity > 0.0 {
                base_attributes
                    .entry(attr_ids::CAPACITY)
                    .or_insert(t.capacity);
            }
            if t.volume > 0.0 {
                base_attributes.entry(161).or_insert(t.volume);
            }
            if t.radius > 0.0 {
                base_attributes.entry(162).or_insert(t.radius);
            }
        }

        Self {
            item_id,
            type_id,
            group_id,
            category_id,
            base_attributes,
            modifiers: Vec::new(),
            skill_levels: HashMap::new(),
        }
    }

    pub fn set_skill_level(&mut self, source_item_id: i64, level: u8) {
        self.skill_levels.insert(source_item_id, level.min(5));
    }

    pub fn add_modifier(&mut self, modifier: AttributeModifier) {
        self.modifiers.push(modifier);
    }

    pub fn remove_modifiers_from_source(&mut self, source_item_id: i64) {
        self.modifiers.retain(|m| m.source_item_id != source_item_id);
    }

    pub fn remove_modifiers_from_source_effect(
        &mut self,
        source_item_id: i64,
        effect_id: Option<EffectId>,
    ) {
        match effect_id {
            Some(eid) => self
                .modifiers
                .retain(|m| !(m.source_item_id == source_item_id && m.effect_id == Some(eid))),
            None => self.remove_modifiers_from_source(source_item_id),
        }
    }

    /// Evaluates the effective value of `attribute_id` after applying all active modifiers
    /// in strict EVE Dogma operator precedence and stacking-penalty order.
    pub fn get_attribute(&self, attribute_id: AttributeId, sde: &SdeRegistry) -> f64 {
        let attr_schema = sde.attributes.get(&attribute_id);
        let mut val = self
            .base_attributes
            .get(&attribute_id)
            .copied()
            .or_else(|| attr_schema.map(|a| a.default_value))
            .unwrap_or(0.0);

        let attr_allows_free_stacking = attr_schema.map(|a| a.stackable).unwrap_or(true);
        let high_is_good = attr_schema.map(|a| a.high_is_good).unwrap_or(true);

        let relevant: Vec<&AttributeModifier> = self
            .modifiers
            .iter()
            .filter(|m| m.target_attribute_id == attribute_id)
            .collect();

        if relevant.is_empty() {
            return val;
        }

        // 1. PreAssign (resolved deterministically by high_is_good)
        let pre_assigns: Vec<f64> = relevant
            .iter()
            .filter(|m| m.operator == ModifierOperator::PreAssign)
            .map(|m| m.value)
            .collect();
        if let Some(best) = select_dominant_assignment(&pre_assigns, high_is_good) {
            val = best;
        }

        // 2. PreMul & PreDiv
        for m in relevant
            .iter()
            .filter(|m| m.operator == ModifierOperator::PreMul)
        {
            val *= m.value;
        }
        for m in relevant
            .iter()
            .filter(|m| m.operator == ModifierOperator::PreDiv)
        {
            if m.value != 0.0 {
                val /= m.value;
            }
        }

        // 3. ModAdd & ModSub
        for m in relevant
            .iter()
            .filter(|m| m.operator == ModifierOperator::ModAdd)
        {
            val += m.value;
        }
        for m in relevant
            .iter()
            .filter(|m| m.operator == ModifierOperator::ModSub)
        {
            val -= m.value;
        }

        // 4. PostMul / PostDiv / PostPercent / PostPercentUnNerfed / SpecialSkillRelationship
        let mut unpenalized_deltas = Vec::new();
        let mut penalized_pos_deltas = Vec::new();
        let mut penalized_neg_deltas = Vec::new();

        for m in &relevant {
            let (delta_opt, force_unpenalized) = match m.operator {
                ModifierOperator::PostMul => (Some(m.value - 1.0), false),
                ModifierOperator::PostDiv if m.value != 0.0 => (Some((1.0 / m.value) - 1.0), false),
                ModifierOperator::PostPercent => (Some(m.value / 100.0), false),
                ModifierOperator::PostPercentUnNerfed => (Some(m.value / 100.0), true),
                ModifierOperator::SpecialSkillRelationship => {
                    let lvl = self
                        .skill_levels
                        .get(&m.source_item_id)
                        .copied()
                        .unwrap_or(1) as f64;
                    (Some((m.value * lvl) / 100.0), true)
                }
                _ => (None, false),
            };
            if let Some(d) = delta_opt {
                let is_penalized =
                    !force_unpenalized && !attr_allows_free_stacking && m.stacking_penalized;
                if !is_penalized {
                    unpenalized_deltas.push(d);
                } else if d >= 0.0 {
                    penalized_pos_deltas.push(d);
                } else {
                    penalized_neg_deltas.push(d);
                }
            }
        }

        for d in unpenalized_deltas {
            val *= 1.0 + d;
        }

        penalized_pos_deltas.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
        for (i, d) in penalized_pos_deltas.into_iter().enumerate() {
            val *= 1.0 + d * stacking_penalty_factor(i);
        }

        penalized_neg_deltas.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        for (i, d) in penalized_neg_deltas.into_iter().enumerate() {
            val *= 1.0 + d * stacking_penalty_factor(i);
        }

        // 5. PostAssign (resolved deterministically by high_is_good)
        let post_assigns: Vec<f64> = relevant
            .iter()
            .filter(|m| m.operator == ModifierOperator::PostAssign)
            .map(|m| m.value)
            .collect();
        if let Some(best) = select_dominant_assignment(&post_assigns, high_is_good) {
            val = best;
        }

        val
    }
}

/// Complete Ship Fitting, Thermodynamics, and Module Activation Engine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShipFittingEngine {
    pub ship: DogmaItemState,
    pub modules: HashMap<i64, FittedModule>,
    /// Heat rack state `[High, Medium, Low]` in `[0.0, 1.0]`.
    pub rack_heat: [f64; 3],
    /// Character trained skill levels (`skill_type_id -> trained_level`) enforced during fitting.
    #[serde(default)]
    pub character_skills: HashMap<TypeId, u8>,
}

impl ShipFittingEngine {
    pub fn from_sde(ship_item_id: i64, ship_type_id: TypeId, sde: &SdeRegistry) -> Self {
        Self {
            ship: DogmaItemState::from_sde(ship_item_id, ship_type_id, sde),
            modules: HashMap::new(),
            rack_heat: [0.0, 0.0, 0.0],
            character_skills: HashMap::new(),
        }
    }

    pub fn set_character_skills(&mut self, skills: HashMap<TypeId, u8>) {
        self.character_skills = skills;
    }

    pub fn with_character_skills(mut self, skills: HashMap<TypeId, u8>) -> Self {
        self.character_skills = skills;
        self
    }

    /// Builds and evaluates a [`DogmaEffectContext`] for the current ship hull, character skills,
    /// and fitted modules over real V24.01 SDE `modifierInfo` records (`DG-1`, `OF-31`).
    pub fn evaluated_context(&self, sde: &SdeRegistry) -> DogmaEffectContext {
        let mut base_ship = self.ship.clone();
        base_ship.modifiers.clear();

        let mut sorted_modules: Vec<FittedModule> = self
            .modules
            .values()
            .filter(|m| m.state != ModuleState::Offline)
            .cloned()
            .collect();
        sorted_modules.sort_by_key(|m| (m.flag_id, m.item_id));

        let dummy = DogmaItemState::from_sde(0, 0, sde);
        let char_state = DogmaItemState::from_sde(-1, 1373, sde);
        let mut ctx = DogmaEffectContext::new(
            dummy.clone(),
            base_ship,
            char_state,
            dummy.clone(),
            dummy.clone(),
            dummy,
        );
        for m in &sorted_modules {
            let mut mod_state = DogmaItemState::from_sde(m.item_id, m.type_id, sde);
            if let Some(ctid) = m.charge_type_id {
                if let Some(charge_type) = sde.types.get(&ctid) {
                    let charge_state = DogmaItemState::from_sde(0, ctid, sde);
                    for &eid in &charge_type.effect_ids {
                        if let Some(effect) = sde.effects.get(&eid) {
                            for spec in &effect.modifiers {
                                if spec.domain == "otherID" {
                                    if let Some(vm) = compile_modifier_spec_to_vm(spec, false) {
                                        let snap = charge_state.clone();
                                        vm.execute_on_target_with_effect(
                                            0,
                                            Some(eid),
                                            &snap,
                                            &mut mod_state,
                                            sde,
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
            }
            ctx.ship_located_items.push(mod_state);
        }

        // 1. Evaluate trained character skills (`category_id == 16`, passive effects `effect_category == 0`).
        //    First evaluate each skill's self-modifiers (`domain == "itemID"`, e.g. `skillLevel` -> `velocityBonus`),
        //    then apply its cross-domain modifiers (`shipID`, `charID`, `LocationGroupModifier`, `LocationRequiredSkillModifier`, `OwnerRequiredSkillModifier`).
        let mut sorted_skills: Vec<(TypeId, u8)> = self
            .character_skills
            .iter()
            .filter(|&(_, &lvl)| lvl > 0)
            .map(|(&tid, &lvl)| (tid, lvl))
            .collect();
        sorted_skills.sort_by_key(|&(tid, _)| tid);

        for (skill_tid, trained_lvl) in sorted_skills {
            let Some(skill_type) = sde.types.get(&skill_tid) else {
                continue;
            };
            if !skill_type.effect_ids.iter().any(|&eid| eid != 132) {
                continue;
            }
            let skill_item_id = i64::from(skill_tid);
            let mut skill_state = DogmaItemState::from_sde(skill_item_id, skill_tid, sde);
            skill_state
                .base_attributes
                .insert(280, f64::from(trained_lvl.min(5)));

            for &eid in &skill_type.effect_ids {
                if eid == 132 {
                    continue;
                }
                if let Some(effect) = sde.effects.get(&eid) {
                    if effect.effect_category == 0 {
                        for spec in &effect.modifiers {
                            if spec.domain == "itemID" {
                                if let Some(vm) = compile_modifier_spec_to_vm(spec, false) {
                                    let snap = skill_state.clone();
                                    vm.execute_on_target_with_effect(
                                        skill_item_id,
                                        Some(eid),
                                        &snap,
                                        &mut skill_state,
                                        sde,
                                    );
                                }
                            }
                        }
                    }
                }
            }

            ctx.item = skill_state;
            for &eid in &skill_type.effect_ids {
                if eid == 132 {
                    continue;
                }
                if let Some(effect) = sde.effects.get(&eid) {
                    if effect.effect_category == 0 {
                        for spec in &effect.modifiers {
                            if spec.domain != "itemID" {
                                ctx.apply_modifier_spec_with_effect(
                                    skill_item_id,
                                    Some(eid),
                                    spec,
                                    sde,
                                );
                            }
                        }
                    }
                }
            }
        }

        // 2. Evaluate ship hull passive effects (`effect_category == 0`).
        if let Some(hull_type) = sde.types.get(&self.ship.type_id) {
            ctx.item = ctx.ship.clone();
            for &eid in &hull_type.effect_ids {
                if let Some(effect) = sde.effects.get(&eid) {
                    if effect.effect_category == 0 {
                        ctx.apply_effect(self.ship.item_id, effect, sde);
                    }
                }
            }
        }

        // 3. For overloaded modules, evaluate self-targeting overload effects (`effect_category == 5`, `domain == "itemID"`,
        //    such as `3175` `overloadSelfSpeedBonus`, `3174` `overloadSelfRangeBonus`, `3200` `overloadSelfArmorDamageAmountDurationBonus`)
        //    onto the module's own state in `ctx.ship_located_items` before evaluating active effects that read those modified module attributes.
        for (idx, m) in sorted_modules.iter().enumerate() {
            if m.state == ModuleState::Overloaded {
                if let Some(mod_type) = sde.types.get(&m.type_id) {
                    for &eid in &mod_type.effect_ids {
                        if let Some(effect) = sde.effects.get(&eid) {
                            if effect.effect_category == 5 {
                                for spec in &effect.modifiers {
                                    if spec.domain == "itemID" {
                                        if let Some(vm) = compile_modifier_spec_to_vm(spec, false) {
                                            let snap = ctx.ship_located_items[idx].clone();
                                            vm.execute_on_target_with_effect(
                                                m.item_id,
                                                Some(eid),
                                                &snap,
                                                &mut ctx.ship_located_items[idx],
                                                sde,
                                            );
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // 4. Evaluate fitted modules' online fitting load (`16` `online` / `2663` `rigSlot`),
        //    passive/online SDE `modifierInfo` effects (`effect_category == 0 | 4`),
        //    and (when `Active` or `Overloaded`) active SDE `modifierInfo` effects (`effect_category == 1..=3`)
        //    plus active propulsion engine effects (`6730` `moduleBonusMicrowarpdrive`, `6731` `moduleBonusAfterburner`).
        for (idx, m) in sorted_modules.iter().enumerate() {
            let Some(mod_type) = sde.types.get(&m.type_id) else {
                continue;
            };
            let mod_state = ctx.ship_located_items[idx].clone();
            ctx.item = mod_state.clone();

            let cpu_need = mod_state.get_attribute(attr_ids::CPU, sde);
            let pg_need = mod_state.get_attribute(attr_ids::POWER, sde);
            let cal_need = mod_state.get_attribute(attr_ids::UPGRADE_COST, sde);

            if cpu_need > 0.0 {
                let expr = DogmaExpressionNode {
                    opcode: DogmaVmOpcode::AddItmMod,
                    value_f64: None,
                    attribute_id: Some(attr_ids::CPU_LOAD),
                    operator: Some(ModifierOperator::ModAdd),
                    arg1: None,
                    arg2: Some(Box::new(DogmaExpressionNode {
                        opcode: DogmaVmOpcode::GetAttr,
                        value_f64: None,
                        attribute_id: Some(attr_ids::CPU),
                        operator: None,
                        arg1: None,
                        arg2: None,
                    })),
                };
                expr.execute_on_target_with_effect(m.item_id, Some(16), &mod_state, &mut ctx.ship, sde);
            }
            if pg_need > 0.0 {
                let expr = DogmaExpressionNode {
                    opcode: DogmaVmOpcode::AddItmMod,
                    value_f64: None,
                    attribute_id: Some(attr_ids::POWER_LOAD),
                    operator: Some(ModifierOperator::ModAdd),
                    arg1: None,
                    arg2: Some(Box::new(DogmaExpressionNode {
                        opcode: DogmaVmOpcode::GetAttr,
                        value_f64: None,
                        attribute_id: Some(attr_ids::POWER),
                        operator: None,
                        arg1: None,
                        arg2: None,
                    })),
                };
                expr.execute_on_target_with_effect(m.item_id, Some(16), &mod_state, &mut ctx.ship, sde);
            }
            if cal_need > 0.0 {
                let expr = DogmaExpressionNode {
                    opcode: DogmaVmOpcode::AddItmMod,
                    value_f64: None,
                    attribute_id: Some(attr_ids::UPGRADE_CAPACITY),
                    operator: Some(ModifierOperator::ModSub),
                    arg1: None,
                    arg2: Some(Box::new(DogmaExpressionNode {
                        opcode: DogmaVmOpcode::GetAttr,
                        value_f64: None,
                        attribute_id: Some(attr_ids::UPGRADE_COST),
                        operator: None,
                        arg1: None,
                        arg2: None,
                    })),
                };
                expr.execute_on_target_with_effect(
                    m.item_id,
                    Some(2663),
                    &mod_state,
                    &mut ctx.ship,
                    sde,
                );
            }

            let is_active = matches!(m.state, ModuleState::Active | ModuleState::Overloaded);
            for &eid in &mod_type.effect_ids {
                if let Some(effect) = sde.effects.get(&eid) {
                    let should_apply = match effect.effect_category {
                        0 | 4 => true,
                        1..=3 => is_active,
                        5 => m.state == ModuleState::Overloaded,
                        _ => false,
                    };
                    if should_apply {
                        for spec in &effect.modifiers {
                            if effect.effect_category == 5 && spec.domain == "itemID" {
                                continue;
                            }
                            ctx.apply_modifier_spec_with_effect(
                                m.item_id,
                                Some(eid),
                                spec,
                                sde,
                            );
                        }
                    }
                    // Engine propulsion effects (`6730` MWD, `6731` Afterburner) have empty `modifiers` in `dogmaeffects.fsdbinary`
                    // and read the module's effective `SPEED_FACTOR` (already modified by `Acceleration Control` `1176` and `overloadSelfSpeedBonus` `3175`)
                    if is_active && matches!(eid, 6730 | 6731) {
                        let mass_attr_id = if mod_type
                            .default_attributes
                            .contains_key(&attr_ids::MASS_ADDITION)
                        {
                            attr_ids::MASS_ADDITION
                        } else {
                            attr_ids::MASS
                        };
                        let expr = DogmaExpressionNode {
                            opcode: DogmaVmOpcode::Combine,
                            value_f64: None,
                            attribute_id: None,
                            operator: None,
                            arg1: Some(Box::new(DogmaExpressionNode {
                                opcode: DogmaVmOpcode::AddItmMod,
                                value_f64: None,
                                attribute_id: Some(attr_ids::MASS),
                                operator: Some(ModifierOperator::ModAdd),
                                arg1: None,
                                arg2: Some(Box::new(DogmaExpressionNode {
                                    opcode: DogmaVmOpcode::GetAttr,
                                    value_f64: None,
                                    attribute_id: Some(mass_attr_id),
                                    operator: None,
                                    arg1: None,
                                    arg2: None,
                                })),
                            })),
                            arg2: Some(Box::new(DogmaExpressionNode {
                                opcode: DogmaVmOpcode::AddItmMod,
                                value_f64: None,
                                attribute_id: Some(attr_ids::MAX_VELOCITY),
                                operator: Some(ModifierOperator::PostPercent),
                                arg1: None,
                                arg2: Some(Box::new(DogmaExpressionNode {
                                    opcode: DogmaVmOpcode::GetAttr,
                                    value_f64: None,
                                    attribute_id: Some(attr_ids::SPEED_FACTOR),
                                    operator: None,
                                    arg1: None,
                                    arg2: None,
                                })),
                            })),
                        };
                        expr.execute_on_target_with_effect(
                            m.item_id,
                            Some(eid),
                            &mod_state,
                            &mut ctx.ship,
                            sde,
                        );
                    }
                }
            }
            ctx.ship_located_items[idx] = ctx.item.clone();
        }

        ctx
    }

    /// Evaluates the ship's passive hull effects, character skill effects (`category_id == 16`),
    /// and fitted module passive/online/active/overload effects through [`DogmaEffectContext`]
    /// over real V24.01 SDE `modifierInfo` records (`DG-1`, `OF-31`).
    pub fn recalculate_stats(&mut self, sde: &SdeRegistry) {
        let mut hi_mod = 0.0;
        let mut med_mod = 0.0;
        let mut low_mod = 0.0;
        let mut tur_mod = 0.0;
        let mut lch_mod = 0.0;
        for m in self.modules.values() {
            if (125..=128).contains(&m.flag_id) {
                if let Some(t) = sde.types.get(&m.type_id) {
                    if let Some(&v) = t.default_attributes.get(&1374) {
                        hi_mod += v;
                    }
                    if let Some(&v) = t.default_attributes.get(&1375) {
                        med_mod += v;
                    }
                    if let Some(&v) = t.default_attributes.get(&1376) {
                        low_mod += v;
                    }
                    if let Some(&v) = t.default_attributes.get(&1368) {
                        tur_mod += v;
                    }
                    if let Some(&v) = t.default_attributes.get(&1369) {
                        lch_mod += v;
                    }
                }
            }
        }
        let hull_group = sde
            .types
            .get(&self.ship.type_id)
            .map(|t| t.group_id)
            .unwrap_or(0);
        if hull_group == 963 {
            self.ship.base_attributes.insert(attr_ids::HI_SLOTS, hi_mod);
            self.ship.base_attributes.insert(attr_ids::MED_SLOTS, med_mod);
            self.ship.base_attributes.insert(attr_ids::LOW_SLOTS, low_mod);
            self.ship
                .base_attributes
                .insert(attr_ids::TURRET_SLOTS_LEFT, tur_mod);
            self.ship
                .base_attributes
                .insert(attr_ids::LAUNCHER_SLOTS_LEFT, lch_mod);
        }

        let ctx = self.evaluated_context(sde);
        self.ship = ctx.ship;
    }

    /// Returns the evaluated [`DogmaItemState`] of a fitted module (including character skill,
    /// ship hull bonus, and overload modifiers applied through [`DogmaEffectContext`]).
    pub fn module_state(&self, module_item_id: i64, sde: &SdeRegistry) -> Option<DogmaItemState> {
        self.evaluated_context(sde)
            .ship_located_items
            .into_iter()
            .find(|m| m.item_id == module_item_id)
    }

    /// Returns the effective evaluated [`DogmaItemState`] of a module (fitted or hypothetical),
    /// including character skill bonuses, overload bonuses, and any loaded charge/script modifiers (`DG-5`, `DG-6`).
    pub fn build_effective_module_state(
        &self,
        module_item_id: i64,
        module_type_id: TypeId,
        _overloaded: bool,
        sde: &SdeRegistry,
    ) -> DogmaItemState {
        if let Some(state) = self.module_state(module_item_id, sde) {
            return state;
        }
        let mut state = DogmaItemState::from_sde(module_item_id, module_type_id, sde);

        let charge_type_id = self
            .modules
            .get(&module_item_id)
            .and_then(|m| m.charge_type_id);

        if let Some(ctid) = charge_type_id {
            if let Some(charge_type) = sde.types.get(&ctid) {
                let charge_state = DogmaItemState::from_sde(0, ctid, sde);
                for &eid in &charge_type.effect_ids {
                    if let Some(effect) = sde.effects.get(&eid) {
                        for spec in &effect.modifiers {
                            if spec.domain == "otherID" {
                                if let Some(vm) = compile_modifier_spec_to_vm(spec, false) {
                                    let snap = charge_state.clone();
                                    vm.execute_on_target_with_effect(
                                        0,
                                        Some(eid),
                                        &snap,
                                        &mut state,
                                        sde,
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
        state
    }

    /// Fits a module into a slot with the specified online state (`online == true` checks CPU & PG,
    /// `online == false` fits into `ModuleState::Offline` without CPU/PG checks).
    pub fn fit_module_with_state(
        &mut self,
        module_item_id: i64,
        module_type_id: TypeId,
        flag_id: u16,
        online: bool,
        sde: &SdeRegistry,
    ) -> Result<(), String> {
        let mod_type = sde
            .types
            .get(&module_type_id)
            .ok_or_else(|| format!("Unknown module type {module_type_id}"))?;
        let hull_type = sde
            .types
            .get(&self.ship.type_id)
            .ok_or_else(|| format!("Unknown ship hull type {}", self.ship.type_id))?;

        if mod_type.category_id != 7 && mod_type.category_id != 32 {
            return Err(format!(
                "Item {} (type {}, category {}) is not a fittable module or subsystem",
                mod_type.name, module_type_id, mod_type.category_id
            ));
        }
        if mod_type.effect_ids.is_empty() {
            return Err(format!(
                "Module {} (type {}) has no fitting slot effect_ids defined",
                mod_type.name, module_type_id
            ));
        }

        if self
            .modules
            .values()
            .any(|m| m.flag_id == flag_id && m.item_id != module_item_id)
        {
            return Err(format!("Fitting slot {flag_id} is already occupied"));
        }

        // Validate subsystem slot (attribute 1366: subSystemSlot)
        if let Some(&req_slot_f) = mod_type.default_attributes.get(&1366) {
            let req_slot = req_slot_f as u16;
            if flag_id != req_slot {
                return Err(format!(
                    "SubsystemSlotMismatch: subsystem {} (type {}) requires slot {req_slot}, cannot fit to flag {flag_id}",
                    mod_type.name, module_type_id
                ));
            }
        }

        // Unconditionally validate module slot-type effect against target flag range
        let (req_eff, eff_name) = match flag_id {
            11..=18 => (11_u16, "loPower"),
            19..=26 => (13_u16, "medPower"),
            27..=34 => (12_u16, "hiPower"),
            92..=94 => (2663_u16, "rigSlot"),
            125..=132 => (3772_u16, "subSystem"),
            _ => return Err(format!("Invalid fitting slot flag {flag_id}")),
        };
        if !mod_type.effect_ids.contains(&req_eff) {
            return Err(format!(
                "Module {} (type {}) lacks required slot effect {} ({eff_name}) for flag {flag_id}",
                mod_type.name, module_type_id, req_eff
            ));
        }

        // Unconditionally validate slot index against ship low/med/high/rig/subsystem slot capacity on the hull
        let (slot_idx, max_slots) = match flag_id {
            11..=18 => (
                flag_id - 11,
                self.ship.get_attribute(attr_ids::LOW_SLOTS, sde).max(0.0) as u16,
            ),
            19..=26 => (
                flag_id - 19,
                self.ship.get_attribute(attr_ids::MED_SLOTS, sde).max(0.0) as u16,
            ),
            27..=34 => (
                flag_id - 27,
                self.ship.get_attribute(attr_ids::HI_SLOTS, sde).max(0.0) as u16,
            ),
            92..=94 => (
                flag_id - 92,
                self.ship.get_attribute(attr_ids::RIG_SLOTS, sde).max(0.0) as u16,
            ),
            125..=132 => (
                flag_id - 125,
                if hull_type.group_id == 963 { 4 } else { 0 },
            ),
            _ => unreachable!(),
        };
        if slot_idx >= max_slots {
            return Err(format!(
                "Slot flag {flag_id} (index {slot_idx}) exceeds hull slot capacity ({max_slots}) on {}",
                hull_type.name
            ));
        }

        // Unconditionally validate turret (effect 42) and launcher (effect 40) hardpoint limits on the ship hull
        if mod_type.effect_ids.contains(&42) {
            let max_turrets = self
                .ship
                .get_attribute(attr_ids::TURRET_SLOTS_LEFT, sde)
                .max(0.0) as usize;
            let fitted_turrets = self
                .modules
                .values()
                .filter(|m| {
                    m.item_id != module_item_id
                        && sde
                            .types
                            .get(&m.type_id)
                            .map(|t| t.effect_ids.contains(&42))
                            .unwrap_or(false)
                })
                .count();
            if fitted_turrets >= max_turrets {
                return Err(format!(
                    "Insufficient turret hardpoints on {}: {fitted_turrets} fitted, hull limit is {max_turrets}",
                    hull_type.name
                ));
            }
        }
        if mod_type.effect_ids.contains(&40) {
            let max_launchers = self
                .ship
                .get_attribute(attr_ids::LAUNCHER_SLOTS_LEFT, sde)
                .max(0.0) as usize;
            let fitted_launchers = self
                .modules
                .values()
                .filter(|m| {
                    m.item_id != module_item_id
                        && sde
                            .types
                            .get(&m.type_id)
                            .map(|t| t.effect_ids.contains(&40))
                            .unwrap_or(false)
                })
                .count();
            if fitted_launchers >= max_launchers {
                return Err(format!(
                    "Insufficient launcher hardpoints on {}: {fitted_launchers} fitted, hull limit is {max_launchers}",
                    hull_type.name
                ));
            }
        }

        // Validate maxGroupFitted (attribute 1544)
        if let Some(&limit_f) = mod_type.default_attributes.get(&1544) {
            let limit = limit_f as usize;
            let group_fitted = self
                .modules
                .values()
                .filter(|m| m.item_id != module_item_id)
                .filter(|m| {
                    sde.types
                        .get(&m.type_id)
                        .map(|t| t.group_id == mod_type.group_id)
                        .unwrap_or(false)
                })
                .count();
            if group_fitted >= limit {
                return Err(format!(
                    "MaxGroupFittedExceeded: module {} (group {}) limit is {limit}, already fitted {group_fitted}",
                    mod_type.name, mod_type.group_id
                ));
            }
        }

        // Validate maxTypeFitted (attribute 2431)
        if let Some(&limit_f) = mod_type.default_attributes.get(&2431) {
            let limit = limit_f as usize;
            let type_fitted = self
                .modules
                .values()
                .filter(|m| m.item_id != module_item_id && m.type_id == module_type_id)
                .count();
            if type_fitted >= limit {
                return Err(format!(
                    "MaxTypeFittedExceeded: module {} (type {}) limit is {limit}, already fitted {type_fitted}",
                    mod_type.name, module_type_id
                ));
            }
        }

        // Validate maxGroupOnline (attribute 978 or group 46) if online
        if online {
            let max_group_online = mod_type
                .default_attributes
                .get(&978)
                .copied()
                .or_else(|| if mod_type.group_id == 46 { Some(1.0) } else { None });
            if let Some(limit) = max_group_online {
                let online_count = self
                    .modules
                    .values()
                    .filter(|m| m.item_id != module_item_id && m.state != ModuleState::Offline)
                    .filter(|m| {
                        sde.types
                            .get(&m.type_id)
                            .map(|t| t.group_id == mod_type.group_id)
                            .unwrap_or(false)
                    })
                    .count();
                if (online_count as f64) >= limit {
                    return Err(format!(
                        "MaxGroupOnlineExceeded: module {} (group {}) limit is {limit}",
                        mod_type.name, mod_type.group_id
                    ));
                }
            }
        }

        // Validate hull group/type restrictions (canFitShipGroup01..20, canFitShipType1..12)
        let group_attrs = [
            1298_u16, 1299, 1300, 1301, 1872, 1879, 1880, 1881, 2065, 2396, 2476, 2477, 2478,
            2479, 2480, 2481, 2482, 2483, 2484, 2485,
        ];
        let type_attrs = [
            1302_u16, 1303, 1304, 1305, 1944, 2103, 2463, 2486, 2487, 2488, 2758, 5948,
        ];
        let allowed_groups: Vec<u32> = group_attrs
            .into_iter()
            .filter_map(|aid| mod_type.default_attributes.get(&aid).copied())
            .filter(|&v| v > 0.0)
            .map(|v| v as u32)
            .collect();
        let allowed_types: Vec<TypeId> = type_attrs
            .into_iter()
            .filter_map(|aid| mod_type.default_attributes.get(&aid).copied())
            .filter(|&v| v > 0.0)
            .map(|v| v as TypeId)
            .collect();
        if (!allowed_groups.is_empty() || !allowed_types.is_empty())
            && !allowed_groups.contains(&hull_type.group_id)
            && !allowed_types.contains(&self.ship.type_id)
        {
            return Err(format!(
                "Module {} (type {}) cannot be fitted to hull {} (type {}, group {}): restricted to groups {:?} or types {:?}",
                mod_type.name,
                module_type_id,
                hull_type.name,
                self.ship.type_id,
                hull_type.group_id,
                allowed_groups,
                allowed_types
            ));
        }

        // Validate rigSize (1547) on rigs against hull rigSize
        let hull_rig_size = self.ship.get_attribute(attr_ids::RIG_SIZE, sde).max(0.0) as u32;
        if mod_type.effect_ids.contains(&2663)
            || mod_type.default_attributes.contains_key(&attr_ids::RIG_SIZE)
        {
            let mod_rig_size = mod_type
                .default_attributes
                .get(&attr_ids::RIG_SIZE)
                .copied()
                .unwrap_or(0.0) as u32;
            if mod_rig_size == 0 || mod_rig_size != hull_rig_size {
                return Err(format!(
                    "Rig size mismatch for {}: module rigSize {mod_rig_size} != hull rigSize {hull_rig_size} on {}",
                    mod_type.name, hull_type.name
                ));
            }
        }

        // Validate required character skills (requiredSkill1..3 and requiredSkill1..3Level)
        for (skill_attr, level_attr) in [
            (attr_ids::REQUIRED_SKILL_1, attr_ids::REQUIRED_SKILL_1_LEVEL),
            (attr_ids::REQUIRED_SKILL_2, attr_ids::REQUIRED_SKILL_2_LEVEL),
            (attr_ids::REQUIRED_SKILL_3, attr_ids::REQUIRED_SKILL_3_LEVEL),
        ] {
            if let Some(&req_skill_f) = mod_type.default_attributes.get(&skill_attr) {
                let req_skill_id = req_skill_f as TypeId;
                if req_skill_id > 0 {
                    let req_level = mod_type
                        .default_attributes
                        .get(&level_attr)
                        .copied()
                        .unwrap_or(1.0)
                        .max(1.0) as u8;
                    let trained_level = self
                        .character_skills
                        .get(&req_skill_id)
                        .copied()
                        .unwrap_or(0);
                    if trained_level < req_level {
                        return Err(format!(
                            "Missing required skill {req_skill_id} level {req_level} (trained: {trained_level}) for module {} (type {})",
                            mod_type.name, module_type_id
                        ));
                    }
                }
            }
        }

        // Validate calibration (for rigs)
        let cal_need = mod_type
            .default_attributes
            .get(&attr_ids::UPGRADE_COST)
            .copied()
            .unwrap_or(0.0);
        if cal_need > 0.0 {
            let current_cal_used: f64 = self
                .modules
                .values()
                .filter(|m| m.item_id != module_item_id && (92..=94).contains(&m.flag_id))
                .filter_map(|m| {
                    sde.types
                        .get(&m.type_id)?
                        .default_attributes
                        .get(&attr_ids::UPGRADE_COST)
                        .copied()
                })
                .sum();
            let cal_capacity = self.ship.get_attribute(attr_ids::UPGRADE_CAPACITY, sde);
            if current_cal_used + cal_need > cal_capacity + 1e-6 {
                return Err(format!(
                    "Insufficient Calibration: need {cal_need}, available {}",
                    cal_capacity - current_cal_used
                ));
            }
        }

        // Prevent duplicate modifiers if already fitted
        if self.modules.contains_key(&module_item_id) {
            self.modules.remove(&module_item_id);
        }
        self.recalculate_stats(sde);

        if online {
            let cpu_need = mod_type
                .default_attributes
                .get(&attr_ids::CPU)
                .copied()
                .unwrap_or(0.0);
            let pg_need = mod_type
                .default_attributes
                .get(&attr_ids::POWER)
                .copied()
                .unwrap_or(0.0);

            let cpu_avail = self.ship.get_attribute(attr_ids::CPU_OUTPUT, sde)
                - self.ship.get_attribute(attr_ids::CPU_LOAD, sde);
            let pg_avail = self.ship.get_attribute(attr_ids::POWER_OUTPUT, sde)
                - self.ship.get_attribute(attr_ids::POWER_LOAD, sde);

            if cpu_need > cpu_avail + 1e-6 {
                return Err(format!("Insufficient CPU: need {cpu_need}, available {cpu_avail}"));
            }
            if pg_need > pg_avail + 1e-6 {
                return Err(format!(
                    "Insufficient Powergrid: need {pg_need}, available {pg_avail}"
                ));
            }
        }

        self.modules.insert(
            module_item_id,
            FittedModule {
                item_id: module_item_id,
                type_id: module_type_id,
                flag_id,
                state: if online {
                    ModuleState::Online
                } else {
                    ModuleState::Offline
                },
                damage_hp: 0.0,
                max_hp: 40.0,
                charge_type_id: None,
                charge_quantity: 0,
            },
        );
        self.recalculate_stats(sde);
        Ok(())
    }

    /// Fits and brings a module online, validating slot effects, hull slot counts, turret/launcher
    /// hardpoints, hull/group restrictions, rig/charge size classes, character skills, and CPU/PG/Calibration.
    pub fn fit_and_online_module(
        &mut self,
        module_item_id: i64,
        module_type_id: TypeId,
        flag_id: u16,
        sde: &SdeRegistry,
    ) -> Result<(), String> {
        self.fit_module_with_state(module_item_id, module_type_id, flag_id, true, sde)
    }

    /// Brings an offline fitted module online, checking maxGroupOnline and CPU/PG.
    pub fn set_module_online(
        &mut self,
        module_item_id: i64,
        sde: &SdeRegistry,
    ) -> Result<(), String> {
        let (mod_type_id, _flag_id) = {
            let m = self
                .modules
                .get(&module_item_id)
                .ok_or_else(|| format!("Module {module_item_id} is not fitted"))?;
            (m.type_id, m.flag_id)
        };
        let mod_type = sde
            .types
            .get(&mod_type_id)
            .ok_or_else(|| format!("Unknown module type {mod_type_id}"))?;

        let max_group_online = mod_type
            .default_attributes
            .get(&978)
            .copied()
            .or_else(|| if mod_type.group_id == 46 { Some(1.0) } else { None });
        if let Some(limit) = max_group_online {
            let online_count = self
                .modules
                .values()
                .filter(|m| m.item_id != module_item_id && m.state != ModuleState::Offline)
                .filter(|m| {
                    sde.types
                        .get(&m.type_id)
                        .map(|t| t.group_id == mod_type.group_id)
                        .unwrap_or(false)
                })
                .count();
            if (online_count as f64) >= limit {
                return Err(format!(
                    "MaxGroupOnlineExceeded: module {} (group {}) limit is {limit}",
                    mod_type.name, mod_type.group_id
                ));
            }
        }

        let cpu_need = mod_type
            .default_attributes
            .get(&attr_ids::CPU)
            .copied()
            .unwrap_or(0.0);
        let pg_need = mod_type
            .default_attributes
            .get(&attr_ids::POWER)
            .copied()
            .unwrap_or(0.0);

        let cpu_avail = self.ship.get_attribute(attr_ids::CPU_OUTPUT, sde)
            - self.ship.get_attribute(attr_ids::CPU_LOAD, sde);
        let pg_avail = self.ship.get_attribute(attr_ids::POWER_OUTPUT, sde)
            - self.ship.get_attribute(attr_ids::POWER_LOAD, sde);

        if cpu_need > cpu_avail + 1e-6 {
            return Err(format!("Insufficient CPU: need {cpu_need}, available {cpu_avail}"));
        }
        if pg_need > pg_avail + 1e-6 {
            return Err(format!(
                "Insufficient Powergrid: need {pg_need}, available {pg_avail}"
            ));
        }

        if let Some(m) = self.modules.get_mut(&module_item_id) {
            m.state = ModuleState::Online;
        }
        self.recalculate_stats(sde);
        Ok(())
    }

    /// Takes an online fitted module offline, removing its CPU/PG and online effects.
    pub fn take_module_offline(
        &mut self,
        module_item_id: i64,
        sde: &SdeRegistry,
    ) -> Result<(), String> {
        let m = self
            .modules
            .get_mut(&module_item_id)
            .ok_or_else(|| format!("Module {module_item_id} is not fitted"))?;
        m.state = ModuleState::Offline;
        self.recalculate_stats(sde);
        Ok(())
    }

    /// Deactivates an active/overloaded module back to `ModuleState::Online`, removing its active-only modifiers.
    pub fn deactivate_module(
        &mut self,
        module_item_id: i64,
        sde: &SdeRegistry,
    ) -> Result<(), String> {
        {
            let mod_entry = self
                .modules
                .get_mut(&module_item_id)
                .ok_or_else(|| format!("Module {module_item_id} is not fitted"))?;
            if mod_entry.state == ModuleState::Offline {
                return Err("Module is offline".into());
            }
            mod_entry.state = ModuleState::Online;
        }
        self.recalculate_stats(sde);
        Ok(())
    }

    /// Activates or overloads an online module (e.g. 1MN Afterburner II), applying its active modifiers
    /// idempotently without stacking duplicates if called repeatedly.
    pub fn activate_module(
        &mut self,
        module_item_id: i64,
        overload: bool,
        sde: &SdeRegistry,
    ) -> Result<(), String> {
        {
            let mod_entry = self
                .modules
                .get_mut(&module_item_id)
                .ok_or_else(|| format!("Module {module_item_id} is not fitted"))?;
            if mod_entry.state == ModuleState::Offline {
                return Err("Cannot activate an offline module".into());
            }
            if mod_entry.damage_hp >= mod_entry.max_hp {
                return Err("Module is burned out (100% heat damage)".into());
            }
            mod_entry.state = if overload {
                ModuleState::Overloaded
            } else {
                ModuleState::Active
            };
        }
        self.recalculate_stats(sde);
        Ok(())
    }
}

/// Thermodynamics formulas (`#29`–`#34`):
/// Rack heat generation, dissipation, slot-distance attenuation $e^{-d}$, and nanite repair paste cost.
pub mod thermodynamics {
    /// Computes rack heat level after `dt_sec` given active heat generation rate and dissipation rate (`#29`–`#30`).
    pub fn step_rack_heat(current_heat: f64, heat_gen_rate: f64, dissipation_rate: f64, dt_sec: f64) -> f64 {
        let target = if heat_gen_rate > 0.0 { 1.0 } else { 0.0 };
        let rate = if heat_gen_rate > 0.0 {
            heat_gen_rate
        } else {
            dissipation_rate
        };
        let next = target + (current_heat - target) * (-rate * dt_sec).exp();
        next.clamp(0.0, 1.0)
    }

    /// Computes slot-distance heat attenuation factor $A(d) = e^{-d_{\text{slots}} \cdot f_{\text{attenuation}}}$ (`#31`).
    pub fn heat_distance_attenuation(slot_distance: usize, attenuation_factor: f64) -> f64 {
        (-(slot_distance as f64) * attenuation_factor).exp()
    }

    /// Computes heat damage probability for a module at slot distance `d` when rack heat is `rack_heat` (`#32`).
    pub fn heat_damage_probability(rack_heat: f64, slot_distance: usize, attenuation_factor: f64) -> f64 {
        (rack_heat * heat_distance_attenuation(slot_distance, attenuation_factor)).clamp(0.0, 1.0)
    }

    /// Computes effective heat damage per cycle after Thermodynamics skill level (`5%` reduction/level, `#33`).
    pub fn effective_heat_damage(base_heat_damage: f64, thermodynamics_level: u8) -> f64 {
        base_heat_damage * (1.0 - 0.05 * (thermodynamics_level.min(5) as f64))
    }

    /// Computes Nanite Repair Paste quantity required to repair `damaged_hp` on a module (`#34`).
    pub fn nanite_paste_required(damaged_hp: f64, paste_per_hp: f64) -> u32 {
        (damaged_hp.max(0.0) * paste_per_hp).ceil() as u32
    }
}

/// Combat Booster Side-Effect Formulas (`#35`–`#36`):
/// Neurotoxin Control reduces side-effect chance (`5%/lvl`); Neurotoxin Recovery reduces severity (`5%/lvl`).
pub fn evaluate_booster_side_effect(
    base_chance: f64,
    base_penalty_pct: f64,
    neurotoxin_control_level: u8,
    neurotoxin_recovery_level: u8,
    deterministic_roll: f64,
) -> Option<f64> {
    let effective_chance = base_chance * (1.0 - 0.05 * (neurotoxin_control_level.min(5) as f64));
    if deterministic_roll < effective_chance {
        let effective_penalty =
            base_penalty_pct * (1.0 - 0.05 * (neurotoxin_recovery_level.min(5) as f64));
        Some(effective_penalty)
    } else {
        None
    }
}

/// Command Burst `dbuff` Aggregation (`#37`): aggregates multiple active `dbuff` values by `Maximum`, `Minimum`, or `Additive`.
pub fn aggregate_dbuff_values(mode: &str, values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    match mode {
        "Minimum" => values
            .iter()
            .copied()
            .min_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
            .unwrap_or(0.0),
        "Additive" => values.iter().sum(),
        _ => values
            .iter()
            .copied()
            .max_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
            .unwrap_or(0.0),
    }
}

/// Abyssal Dynamic Item (`mutaplasmid`) Attribute Rolling (`#38`):
/// Applies linear interpolation $A_{\text{rolled}} = A_{\text{base}} \cdot (M_{\min} + u \cdot (M_{\max} - M_{\min}))$.
pub fn roll_abyssal_attributes(
    base_attributes: &HashMap<AttributeId, f64>,
    ranges: &HashMap<AttributeId, [f64; 2]>,
    unit_rolls: &HashMap<AttributeId, f64>,
) -> HashMap<AttributeId, f64> {
    let mut out = base_attributes.clone();
    for (&attr_id, &[min_mult, max_mult]) in ranges {
        if let Some(&base_val) = base_attributes.get(&attr_id) {
            let u = unit_rolls.get(&attr_id).copied().unwrap_or(0.5).clamp(0.0, 1.0);
            let mult = min_mult + u * (max_mult - min_mult);
            out.insert(attr_id, base_val * mult);
        }
    }
    out
}

fn select_dominant_assignment(values: &[f64], high_is_good: bool) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    if high_is_good {
        values
            .iter()
            .copied()
            .max_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
    } else {
        values
            .iter()
            .copied()
            .min_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eve_sde::attr_ids;

    #[test]
    fn test_sde_operator_and_vm_opcode_mapping() {
        assert_eq!(
            ModifierOperator::try_from_sde_op(-1),
            Some(ModifierOperator::PreAssign)
        );
        assert_eq!(
            ModifierOperator::try_from_sde_op(8),
            Some(ModifierOperator::PostPercentUnNerfed)
        );
        assert_eq!(
            ModifierOperator::try_from_sde_op(9),
            Some(ModifierOperator::SpecialSkillRelationship)
        );
        assert_eq!(DogmaVmOpcode::from_u8(1), Some(DogmaVmOpcode::Add));
        assert_eq!(DogmaVmOpcode::from_u8(75), Some(DogmaVmOpcode::TransferCap));
        assert_eq!(DogmaVmOpcode::from_u8(76), None);
    }

    #[test]
    fn test_stacking_penalty_coefficients() {
        assert!((stacking_penalty_factor(0) - 1.0).abs() < 1e-6);
        assert!((stacking_penalty_factor(1) - 0.869118).abs() < 1e-4);
        assert!((stacking_penalty_factor(2) - 0.570583).abs() < 1e-4);
        assert!((stacking_penalty_factor(3) - 0.282955).abs() < 1e-4);
    }

    #[test]
    fn test_rifter_velocity_with_overdrives_and_skills() {
        let sde = SdeRegistry::with_canonical_seed();
        let mut rifter = DogmaItemState::from_sde(1001, 587, &sde);

        assert_eq!(rifter.get_attribute(attr_ids::MAX_VELOCITY, &sde), 365.0);

        rifter.add_modifier(AttributeModifier {
            source_item_id: 2001,
            effect_id: None,
            target_attribute_id: attr_ids::MAX_VELOCITY,
            operator: ModifierOperator::PostPercent,
            value: 25.0,
            stacking_penalized: false,
        });

        for id in [3001, 3002] {
            rifter.add_modifier(AttributeModifier {
                source_item_id: id,
                effect_id: None,
                target_attribute_id: attr_ids::MAX_VELOCITY,
                operator: ModifierOperator::PostPercent,
                value: 12.5,
                stacking_penalized: true,
            });
        }

        let effective_vel = rifter.get_attribute(attr_ids::MAX_VELOCITY, &sde);
        let expected = 365.0 * 1.25 * (1.0 + 0.125) * (1.0 + 0.125 * stacking_penalty_factor(1));
        assert!((effective_vel - expected).abs() < 1e-6);
    }

    #[test]
    fn test_dogma_effect_context_domain_isolation_and_semantics() {
        let sde = SdeRegistry::with_canonical_seed();

        // 1. LocationModifier must modify ONLY items located in the target domain, NOT the location entity itself,
        //    and per-domain location lists (shipID vs structureID vs charID vs targetID) must be strictly isolated.
        let mut src_mod = DogmaItemState::from_sde(100, 1236, &sde); // Overdrive II (Module, category 7)
        src_mod.base_attributes.insert(20, 25.0);
        let mut ship = DogmaItemState::from_sde(1, 587, &sde);
        ship.base_attributes.insert(37, 200.0);
        let mut structure = DogmaItemState::from_sde(2, 35832, &sde);
        structure.base_attributes.insert(37, 200.0);

        let mut ctx = DogmaEffectContext::new(
            src_mod,
            ship,
            DogmaItemState::from_sde(3, 1373, &sde),
            DogmaItemState::from_sde(4, 587, &sde),
            DogmaItemState::from_sde(5, 587, &sde),
            structure,
        );
        let mut ship_child = DogmaItemState::from_sde(10, 438, &sde);
        ship_child.base_attributes.insert(37, 100.0);
        let mut struct_child = DogmaItemState::from_sde(11, 438, &sde);
        struct_child.base_attributes.insert(37, 100.0);
        ctx.ship_located_items.push(ship_child);
        ctx.structure_located_items.push(struct_child);

        let loc_spec = EffectModifierSpec {
            domain: "shipID".into(),
            func: "LocationModifier".into(),
            modified_attribute_id: 37,
            modifying_attribute_id: 20,
            operator: 6, // PostPercent (+25%)
            group_id: None,
            skill_type_id: None,
            effect_id: None,
        };
        assert_eq!(ctx.apply_modifier_spec_with_effect(100, Some(9001), &loc_spec, &sde), 1);
        // Ship itself must NOT be modified by LocationModifier
        assert_eq!(ctx.ship.get_attribute(37, &sde), 200.0);
        // Item located in shipID is modified (+25% -> 125.0)
        assert!((ctx.ship_located_items[0].get_attribute(37, &sde) - 125.0).abs() < 1e-6);
        // Item located in structureID is isolated and untouched (100.0)
        assert_eq!(ctx.structure_located_items[0].get_attribute(37, &sde), 100.0);

        // 2. LocationRequiredSkillModifier must NOT apply to items that have no required skills
        let mut no_skill_item = DogmaItemState {
            item_id: 12,
            type_id: 0,
            group_id: Some(10),
            category_id: Some(7),
            base_attributes: HashMap::from([(51, 1000.0)]),
            modifiers: Vec::new(),
            skill_levels: HashMap::new(),
        };
        let srq_spec = EffectModifierSpec {
            domain: "shipID".into(),
            func: "LocationRequiredSkillModifier".into(),
            modified_attribute_id: 51,
            modifying_attribute_id: 20,
            operator: 6,
            group_id: None,
            skill_type_id: Some(3319),
            effect_id: None,
        };
        let srq_vm = compile_modifier_spec_to_vm(&srq_spec, false).unwrap();
        srq_vm.execute_on_target(100, &ctx.item, &mut no_skill_item, &sde);
        assert!(
            no_skill_item.modifiers.is_empty(),
            "Item with no required skills must not match LocationRequiredSkillModifier"
        );

        // 3. LocationGroupModifier must NOT apply when the target item's group is unknown (0 / None)
        let mut unknown_grp_item = DogmaItemState {
            item_id: 13,
            type_id: 0,
            group_id: None,
            category_id: None,
            base_attributes: HashMap::from([(64, 100.0)]),
            modifiers: Vec::new(),
            skill_levels: HashMap::new(),
        };
        let grp_spec = EffectModifierSpec {
            domain: "shipID".into(),
            func: "LocationGroupModifier".into(),
            modified_attribute_id: 64,
            modifying_attribute_id: 20,
            operator: 6,
            group_id: Some(55),
            skill_type_id: None,
            effect_id: None,
        };
        let grp_vm = compile_modifier_spec_to_vm(&grp_spec, false).unwrap();
        grp_vm.execute_on_target(100, &ctx.item, &mut unknown_grp_item, &sde);
        assert!(
            unknown_grp_item.modifiers.is_empty(),
            "Item with unknown group must not match LocationGroupModifier"
        );

        // 4. Per-effect removal: removing one effect from source_item_id must leave other effects from the same source intact
        let eff_a = DogmaEffectType {
            effect_id: 9010,
            name: "effA".into(),
            effect_category: 4,
            is_offensive: false,
            is_assistance: false,
            modifiers: vec![EffectModifierSpec {
                domain: "shipID".into(),
                func: "ItemModifier".into(),
                modified_attribute_id: 37,
                modifying_attribute_id: 20,
                operator: 2, // ModAdd (+25)
                group_id: None,
                skill_type_id: None,
                effect_id: None,
            }],
        };
        let eff_b = DogmaEffectType {
            effect_id: 9011,
            name: "effB".into(),
            effect_category: 4,
            is_offensive: false,
            is_assistance: false,
            modifiers: vec![EffectModifierSpec {
                domain: "shipID".into(),
                func: "ItemModifier".into(),
                modified_attribute_id: 37,
                modifying_attribute_id: 20,
                operator: 6, // PostPercent (+25%)
                group_id: None,
                skill_type_id: None,
                effect_id: None,
            }],
        };
        ctx.apply_effect(100, &eff_a, &sde);
        ctx.apply_effect(100, &eff_b, &sde);
        assert_eq!(ctx.ship.modifiers.len(), 2);
        assert!((ctx.ship.get_attribute(37, &sde) - (200.0 + 25.0) * 1.25).abs() < 1e-6);

        // Remove only eff_a; eff_b from the same source_item_id (100) must remain active!
        ctx.remove_effect(100, &eff_a);
        assert_eq!(ctx.ship.modifiers.len(), 1);
        assert_eq!(ctx.ship.modifiers[0].effect_id, Some(9011));
        assert!((ctx.ship.get_attribute(37, &sde) - 200.0 * 1.25).abs() < 1e-6);

        // 5. Source-category stacking-penalty exemptions:
        //    Skills (category 16), Ships (category 6), Implants (category 20), Subsystems (category 32),
        //    and Boosters (group 303) are exempt (stacking_penalized == false),
        //    whereas Modules (category 7) are stacking-penalized (stacking_penalized == true).
        let mut skill_src = DogmaItemState::from_sde(201, 3449, &sde); // Navigation (Skill, category 16)
        skill_src.base_attributes.insert(20, 25.0);
        let mut ship_src = DogmaItemState::from_sde(202, 587, &sde); // Rifter (Ship, category 6)
        ship_src.base_attributes.insert(20, 25.0);
        let implant_src = DogmaItemState {
            item_id: 203,
            type_id: 0,
            group_id: Some(300),
            category_id: Some(20), // Implant (category 20)
            base_attributes: HashMap::from([(20, 25.0)]),
            modifiers: Vec::new(),
            skill_levels: HashMap::new(),
        };
        let subsys_src = DogmaItemState {
            item_id: 204,
            type_id: 0,
            group_id: Some(954),
            category_id: Some(32), // Subsystem (category 32)
            base_attributes: HashMap::from([(20, 25.0)]),
            modifiers: Vec::new(),
            skill_levels: HashMap::new(),
        };
        let booster_src = DogmaItemState {
            item_id: 205,
            type_id: 0,
            group_id: Some(303), // Booster (group 303)
            category_id: Some(20),
            base_attributes: HashMap::from([(20, 25.0)]),
            modifiers: Vec::new(),
            skill_levels: HashMap::new(),
        };
        let mut target_ship = DogmaItemState::from_sde(1, 587, &sde);
        let pct_vm = compile_modifier_spec_to_vm(&eff_b.modifiers[0], false).unwrap();
        pct_vm.execute_on_target_with_effect(201, Some(9011), &skill_src, &mut target_ship, &sde);
        pct_vm.execute_on_target_with_effect(202, Some(9012), &ship_src, &mut target_ship, &sde);
        pct_vm.execute_on_target_with_effect(203, Some(9013), &implant_src, &mut target_ship, &sde);
        pct_vm.execute_on_target_with_effect(204, Some(9014), &subsys_src, &mut target_ship, &sde);
        pct_vm.execute_on_target_with_effect(205, Some(9015), &booster_src, &mut target_ship, &sde);
        pct_vm.execute_on_target_with_effect(100, Some(9016), &ctx.item, &mut target_ship, &sde);
        assert!(!target_ship.modifiers[0].stacking_penalized);
        assert!(!target_ship.modifiers[1].stacking_penalized);
        assert!(!target_ship.modifiers[2].stacking_penalized);
        assert!(!target_ship.modifiers[3].stacking_penalized);
        assert!(!target_ship.modifiers[4].stacking_penalized);
        assert!(target_ship.modifiers[5].stacking_penalized);

        // Module modifying a stackable attribute (38 `capacity`, stackable == true) must have stacking_penalized == false
        let cap_spec = EffectModifierSpec {
            domain: "shipID".into(),
            func: "ItemModifier".into(),
            modified_attribute_id: 38,
            modifying_attribute_id: 20,
            operator: 6,
            group_id: None,
            skill_type_id: None,
            effect_id: None,
        };
        let cap_vm = compile_modifier_spec_to_vm(&cap_spec, false).unwrap();
        cap_vm.execute_on_target_with_effect(100, Some(9017), &ctx.item, &mut target_ship, &sde);
        assert!(!target_ship.modifiers[6].stacking_penalized);

        // Multi-attribute effect modifying 3 distinct non-stackable attributes (37 maxVelocity, 70 agility, 552 signatureRadius)
        // must evaluate rank 0 (1.0 penalty factor) per attribute independently without cross-attribute penalty contamination.
        let mut multi_attr_ship = DogmaItemState::from_sde(99, 587, &sde);
        multi_attr_ship.base_attributes.insert(37, 100.0);
        multi_attr_ship.base_attributes.insert(70, 100.0);
        multi_attr_ship.base_attributes.insert(552, 100.0);
        for (idx, attr_id) in [37_u16, 70, 552].into_iter().enumerate() {
            let spec = EffectModifierSpec {
                domain: "shipID".into(),
                func: "ItemModifier".into(),
                modified_attribute_id: attr_id,
                modifying_attribute_id: 20,
                operator: 6, // +25%
                group_id: None,
                skill_type_id: None,
                effect_id: None,
            };
            let vm = compile_modifier_spec_to_vm(&spec, false).unwrap();
            vm.execute_on_target_with_effect(
                100,
                Some(9030 + idx as u16),
                &ctx.item,
                &mut multi_attr_ship,
                &sde,
            );
        }
        assert!((multi_attr_ship.get_attribute(37, &sde) - 125.0).abs() < 1e-6);
        assert!((multi_attr_ship.get_attribute(70, &sde) - 125.0).abs() < 1e-6);
        assert!((multi_attr_ship.get_attribute(552, &sde) - 125.0).abs() < 1e-6);

        // 6. OwnerRequiredSkillModifier must modify ONLY items in ctx.owned_items that require
        //    the specified skill, and NEVER modify ship_located_items, ship, character, or owned items
        //    that do not require that skill.
        let mut owned_matching = DogmaItemState::from_sde(301, 438, &sde); // 1MN Afterburner II (requires 3450 Afterburner)
        owned_matching.base_attributes.insert(73, 10_000.0);
        let mut owned_non_matching = DogmaItemState::from_sde(302, 1236, &sde); // Overdrive II (does NOT require 3450)
        owned_non_matching.base_attributes.insert(73, 10_000.0);
        let mut unowned_ship_located = DogmaItemState::from_sde(303, 438, &sde); // Same type 438, but in ship_located_items only
        unowned_ship_located.base_attributes.insert(73, 10_000.0);

        let mut char_src = DogmaItemState::from_sde(300, 3450, &sde); // Afterburner skill
        char_src.base_attributes.insert(20, -10.0);
        let mut owner_ctx = DogmaEffectContext::new(
            char_src,
            DogmaItemState::from_sde(1, 587, &sde),
            DogmaItemState::from_sde(3, 1373, &sde),
            DogmaItemState::from_sde(4, 587, &sde),
            DogmaItemState::from_sde(5, 587, &sde),
            DogmaItemState::from_sde(2, 35832, &sde),
        );
        owner_ctx.ship.base_attributes.insert(73, 10_000.0);
        owner_ctx.character.base_attributes.insert(73, 10_000.0);
        owner_ctx.ship_located_items.push(unowned_ship_located);
        owner_ctx.owned_items.push(owned_matching);
        owner_ctx.owned_items.push(owned_non_matching);

        let orq_spec = EffectModifierSpec {
            domain: "charID".into(),
            func: "OwnerRequiredSkillModifier".into(),
            modified_attribute_id: 73,
            modifying_attribute_id: 20,
            operator: 6, // PostPercent (-10%)
            group_id: None,
            skill_type_id: Some(3450),
            effect_id: None,
        };
        assert_eq!(
            owner_ctx.apply_modifier_spec_with_effect(300, Some(9020), &orq_spec, &sde),
            1
        );
        assert!((owner_ctx.owned_items[0].get_attribute(73, &sde) - 9_000.0).abs() < 1e-6);
        assert_eq!(owner_ctx.owned_items[1].get_attribute(73, &sde), 10_000.0);
        assert_eq!(owner_ctx.ship_located_items[0].get_attribute(73, &sde), 10_000.0);
        assert_eq!(owner_ctx.ship.get_attribute(73, &sde), 10_000.0);
        assert_eq!(owner_ctx.character.get_attribute(73, &sde), 10_000.0);
    }

    #[test]
    // parity: plan:DG-1, feat:dogma.modifier-engine, feat:fitting.stacking-penalty, feat:skills.skill-bonus-application, feat:modules.nanofiber-overdrive
    fn test_ship_fitting_thermodynamics_boosters_and_abyssal_roll() {
        eve_testkit::attest(&[
            "feat:dogma.modifier-engine",
            "feat:fitting.stacking-penalty",
            "feat:skills.skill-bonus-application",
            "feat:modules.nanofiber-overdrive",
        ]);
        let sde = SdeRegistry::with_canonical_seed();

        // 1. Prove Rifter (587) with Navigation V (3449 -> 5) and 2x Overdrive Injector System II (1236)
        //    fitted through ShipFittingEngine matches exact published attribute values:
        //    - Base Rifter maxVelocity (37) = 365.0 m/s, capacity (38) = 140.0 m3
        //    - Navigation V (category 16 skill, unpenalized): +25% maxVelocity (5% * level 5)
        //    - 2x Overdrive Injector System II (category 7 module, effects 59 & 2865):
        //      +12.5% maxVelocity (stacking-penalized across the 2 modules)
        //      and -20% cargo capacity (PostMul 0.8 * 0.8, unpenalized)
        {
            let mut rifter_fit = ShipFittingEngine::from_sde(1000, 587, &sde);
            rifter_fit.set_character_skills(HashMap::from([
                (3449, 5), // Navigation V
                (3394, 2), // Hull Upgrades II
            ]));
            rifter_fit.fit_and_online_module(4001, 1236, 11, &sde).unwrap();
            rifter_fit.fit_and_online_module(4002, 1236, 12, &sde).unwrap();

            let expected_vel =
                365.0 * 1.25 * (1.0 + 0.125) * (1.0 + 0.125 * stacking_penalty_factor(1));
            let actual_vel = rifter_fit.ship.get_attribute(attr_ids::MAX_VELOCITY, &sde);
            assert!(
                (actual_vel - expected_vel).abs() < 1e-6,
                "Rifter + Navigation V + 2x Overdrive II maxVelocity mismatch: got {actual_vel}, expected {expected_vel}"
            );

            let expected_cap = 140.0 * 0.8 * 0.8;
            let actual_cap = rifter_fit.ship.get_attribute(attr_ids::CAPACITY, &sde);
            assert!(
                (actual_cap - expected_cap).abs() < 1e-6,
                "Rifter + 2x Overdrive II cargo capacity mismatch: got {actual_cap}, expected {expected_cap}"
            );

            // Fit Nanofiber Internal Structure II (2605) in slot 13 and Inertial Stabilizers II (1405) in slot 12
            // to verify all 3 passive speed/agility low-slot module families (Overdrive, Nanofiber, Inertial Stabilizer)
            rifter_fit.fit_and_online_module(4002, 1405, 12, &sde).unwrap();
            rifter_fit.fit_and_online_module(4003, 2605, 13, &sde).unwrap();

            let base_agility = sde.types[&587].default_attributes[&attr_ids::AGILITY];
            let base_hull_hp = sde.types[&587].default_attributes[&attr_ids::HP_STRUCT];
            let base_sig = sde.types[&587].default_attributes[&attr_ids::SIGNATURE_RADIUS];

            // Velocity: Navigation V (+25% unpenalized), Overdrive II (+12.5% rank 0), Nanofiber II (+9.5% rank 1)
            let expected_mixed_vel =
                365.0 * 1.25 * (1.0 + 0.125) * (1.0 + 0.095 * stacking_penalty_factor(1));
            assert!(
                (rifter_fit.ship.get_attribute(attr_ids::MAX_VELOCITY, &sde) - expected_mixed_vel)
                    .abs()
                    < 1e-6
            );

            // Agility: Inertial Stabilizers II (-20.0% rank 0), Nanofiber II (-15.75% rank 1)
            let expected_agility =
                base_agility * (1.0 - 0.20) * (1.0 - 0.1575 * stacking_penalty_factor(1));
            assert!(
                (rifter_fit.ship.get_attribute(attr_ids::AGILITY, &sde) - expected_agility).abs()
                    < 1e-6
            );

            // Structure HP: Nanofiber II (PostMul 0.8)
            assert!(
                (rifter_fit.ship.get_attribute(attr_ids::HP_STRUCT, &sde) - base_hull_hp * 0.8)
                    .abs()
                    < 1e-6
            );

            // Signature radius: Inertial Stabilizers II (+11% PostPercent)
            assert!(
                (rifter_fit.ship.get_attribute(attr_ids::SIGNATURE_RADIUS, &sde)
                    - base_sig * 1.11)
                    .abs()
                    < 1e-6
            );
        }

        let mut fit = ShipFittingEngine::from_sde(1001, 587, &sde);

        // Fitting without trained skills must fail
        assert!(fit.fit_and_online_module(5001, 438, 19, &sde).is_err());

        fit.set_character_skills(HashMap::from([
            (3449, 4), // Navigation IV
            (3450, 4), // Afterburner IV
            (3452, 4), // Acceleration Control IV
            (3394, 2), // Hull Upgrades II
            (3300, 5), // Gunnery V
            (3302, 5), // Small Projectile Turret V
            (33094, 5), // Medium Precursor Weapon V
        ]));

        // Heavy Entropic Disintegrator II (47918) is restricted to Triglavian cruisers/HACs and must be rejected on Rifter (587)
        assert!(fit.fit_and_online_module(5099, 47918, 27, &sde).is_err());

        // Fit 1MN Afterburner II (type 438) and Overdrive Injector System II (type 1236)
        fit.fit_and_online_module(5001, 438, 19, &sde).unwrap();
        fit.fit_and_online_module(5002, 1236, 11, &sde).unwrap();
        fit.activate_module(5001, true, &sde).unwrap();

        assert!(fit.ship.get_attribute(attr_ids::MAX_VELOCITY, &sde) > 800.0);
        assert_eq!(fit.ship.get_attribute(attr_ids::CPU_LOAD, &sde), 15.0);

        // Verify LocationRequiredSkillModifier from trained Afterburner IV (3450 -> 4, -5%/lvl duration)
        // and Acceleration Control IV (3452 -> 4, +5%/lvl speedFactor) plus overloadSelfSpeedBonus (3175, +50% speedFactor)
        // were applied to the fitted Afterburner II (5001)
        let fitted_ab_ovl = fit.module_state(5001, &sde).unwrap();
        assert!((fitted_ab_ovl.get_attribute(attr_ids::DURATION, &sde) - 10_000.0 * 0.8).abs() < 1e-6);
        assert!((fitted_ab_ovl.get_attribute(attr_ids::SPEED_FACTOR, &sde) - 135.0 * 1.2 * 1.5).abs() < 1e-6);

        // Deactivate 1MN Afterburner II and verify active/overload effects are removed cleanly while passive/online effects remain
        fit.deactivate_module(5001, &sde).unwrap();
        let fitted_ab_online = fit.module_state(5001, &sde).unwrap();
        assert!((fitted_ab_online.get_attribute(attr_ids::SPEED_FACTOR, &sde) - 135.0 * 1.2).abs() < 1e-6);
        let expected_passive_vel = 365.0 * 1.20 * 1.125;
        assert!(
            (fit.ship.get_attribute(attr_ids::MAX_VELOCITY, &sde) - expected_passive_vel).abs() < 1e-6
        );

        // Thermodynamics check
        let heat = thermodynamics::step_rack_heat(0.0, 0.04, 0.01, 10.0);
        assert!(heat > 0.3 && heat < 0.4);
        assert_eq!(thermodynamics::effective_heat_damage(1.4, 5), 1.4 * 0.75);

        // Booster side-effect check
        let penalty = evaluate_booster_side_effect(0.4, -20.0, 5, 5, 0.1).unwrap();
        assert!((penalty - (-15.0)).abs() < 1e-6);

        // Abyssal mutaplasmid roll check (47748 = Gravid 1MN Afterburner Mutaplasmid, 0.95..1.15x SPEED_FACTOR)
        let ab = sde.types.get(&438).unwrap();
        let muta = sde.dynamic_item_attributes.get(&47748).unwrap();
        let mut rolls = HashMap::new();
        rolls.insert(attr_ids::SPEED_FACTOR, 1.0); // Max roll (1.15x)
        let rolled = roll_abyssal_attributes(&ab.default_attributes, &muta.attribute_ranges, &rolls);
        assert!((rolled[&attr_ids::SPEED_FACTOR] - 135.0 * 1.15).abs() < 1e-4);
    }
}
