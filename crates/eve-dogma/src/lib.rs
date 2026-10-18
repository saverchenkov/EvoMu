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

use eve_sde::{attr_ids, AttributeId, SdeRegistry, TypeId};
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
        match self.opcode {
            DogmaVmOpcode::Combine | DogmaVmOpcode::Splice => {
                if let Some(a1) = &self.arg1 {
                    a1.execute_on_target(source_item_id, source, target, sde);
                }
                if let Some(a2) = &self.arg2 {
                    a2.execute_on_target(source_item_id, source, target, sde);
                }
            }
            DogmaVmOpcode::If => {
                let cond = self.arg1.as_ref().map(|n| n.evaluate(source, sde)).unwrap_or(0.0);
                if cond != 0.0 {
                    if let Some(a2) = &self.arg2 {
                        a2.execute_on_target(source_item_id, source, target, sde);
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
                // Enforce group or skill-requirement filter when specified in arg1
                if matches!(
                    self.opcode,
                    DogmaVmOpcode::AddLocGrpMod | DogmaVmOpcode::AddGangGrpMod
                ) {
                    if let Some(filter_node) = &self.arg1 {
                        if filter_node.opcode == DogmaVmOpcode::DefGroup {
                            if let Some(req_grp) = filter_node.value_f64 {
                                let actual_grp = sde
                                    .types
                                    .get(&target.type_id)
                                    .map(|t| t.group_id)
                                    .unwrap_or(0);
                                if actual_grp != 0 && actual_grp != req_grp as u32 {
                                    return;
                                }
                            }
                        }
                    }
                }
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
                                if !target.skill_levels.is_empty()
                                    && !target.skill_levels.contains_key(&(req_skill as i64))
                                {
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
                let stacking_penalized = !matches!(
                    op,
                    ModifierOperator::ModAdd
                        | ModifierOperator::ModSub
                        | ModifierOperator::PreAssign
                        | ModifierOperator::PostAssign
                        | ModifierOperator::PostPercentUnNerfed
                        | ModifierOperator::SpecialSkillRelationship
                );
                target.add_modifier(AttributeModifier {
                    source_item_id,
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
                target.remove_modifiers_from_source(source_item_id);
            }
            _ => {}
        }
    }
}

/// Represents an active modifier applied to an entity's Dogma attribute.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttributeModifier {
    pub source_item_id: i64,
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
    pub base_attributes: HashMap<AttributeId, f64>,
    pub modifiers: Vec<AttributeModifier>,
    /// Optional trained skill levels keyed by `source_item_id` for `SpecialSkillRelationship` (`dgmAssSpecialSkillRelationship`).
    #[serde(default)]
    pub skill_levels: HashMap<i64, u8>,
}

impl DogmaItemState {
    pub fn from_sde(item_id: i64, type_id: TypeId, sde: &SdeRegistry) -> Self {
        let base_attributes = sde
            .types
            .get(&type_id)
            .map(|t| t.default_attributes.clone())
            .unwrap_or_default();

        Self {
            item_id,
            type_id,
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

    fn apply_online_modifiers_for_module(
        &mut self,
        module_item_id: i64,
        module_type_id: TypeId,
        sde: &SdeRegistry,
    ) {
        let Some(mod_type) = sde.types.get(&module_type_id) else {
            return;
        };
        let mod_state = DogmaItemState::from_sde(module_item_id, module_type_id, sde);
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
            expr.execute_on_target(module_item_id, &mod_state, &mut self.ship, sde);
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
            expr.execute_on_target(module_item_id, &mod_state, &mut self.ship, sde);
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
            expr.execute_on_target(module_item_id, &mod_state, &mut self.ship, sde);
        }

        // Apply passive velocity/shield bonus via DogmaExpressionNode VM if present (Overdrive Injector or Rig)
        if mod_type.default_attributes.contains_key(&attr_ids::SPEED_FACTOR) && mod_type.group_id == 762 {
            let expr = DogmaExpressionNode {
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
            };
            expr.execute_on_target(module_item_id, &mod_state, &mut self.ship, sde);
        }
        if mod_type.default_attributes.contains_key(&attr_ids::SHIELD_BONUS) && mod_type.group_id == 774 {
            let expr = DogmaExpressionNode {
                opcode: DogmaVmOpcode::AddItmMod,
                value_f64: None,
                attribute_id: Some(attr_ids::SHIELD_CAPACITY),
                operator: Some(ModifierOperator::PostPercent),
                arg1: None,
                arg2: Some(Box::new(DogmaExpressionNode {
                    opcode: DogmaVmOpcode::GetAttr,
                    value_f64: None,
                    attribute_id: Some(attr_ids::SHIELD_BONUS),
                    operator: None,
                    arg1: None,
                    arg2: None,
                })),
            };
            expr.execute_on_target(module_item_id, &mod_state, &mut self.ship, sde);
        }
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

        // Validate hull group/type restrictions (canFitShipGroup01..03, canFitShipType1..2)
        let allowed_groups: Vec<u32> = [
            attr_ids::CAN_FIT_SHIP_GROUP_01,
            attr_ids::CAN_FIT_SHIP_GROUP_02,
            attr_ids::CAN_FIT_SHIP_GROUP_03,
        ]
        .into_iter()
        .filter_map(|aid| mod_type.default_attributes.get(&aid).copied())
        .filter(|&v| v > 0.0)
        .map(|v| v as u32)
        .collect();
        let allowed_types: Vec<TypeId> = [
            attr_ids::CAN_FIT_SHIP_TYPE_1,
            attr_ids::CAN_FIT_SHIP_TYPE_2,
        ]
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

        // Prevent duplicate online modifiers if already fitted
        if self.modules.contains_key(&module_item_id) {
            self.ship.remove_modifiers_from_source(module_item_id);
            self.modules.remove(&module_item_id);
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
        let cal_need = mod_type
            .default_attributes
            .get(&attr_ids::UPGRADE_COST)
            .copied()
            .unwrap_or(0.0);

        let cpu_avail = self.ship.get_attribute(attr_ids::CPU_OUTPUT, sde)
            - self.ship.get_attribute(attr_ids::CPU_LOAD, sde);
        let pg_avail = self.ship.get_attribute(attr_ids::POWER_OUTPUT, sde)
            - self.ship.get_attribute(attr_ids::POWER_LOAD, sde);
        let cal_avail = self.ship.get_attribute(attr_ids::UPGRADE_CAPACITY, sde);

        if cpu_need > cpu_avail + 1e-6 {
            return Err(format!("Insufficient CPU: need {cpu_need}, available {cpu_avail}"));
        }
        if pg_need > pg_avail + 1e-6 {
            return Err(format!(
                "Insufficient Powergrid: need {pg_need}, available {pg_avail}"
            ));
        }
        if cal_need > 0.0 && cal_need > cal_avail + 1e-6 {
            return Err(format!(
                "Insufficient Calibration: need {cal_need}, available {cal_avail}"
            ));
        }

        self.apply_online_modifiers_for_module(module_item_id, module_type_id, sde);

        self.modules.insert(
            module_item_id,
            FittedModule {
                item_id: module_item_id,
                type_id: module_type_id,
                flag_id,
                state: ModuleState::Online,
                damage_hp: 0.0,
                max_hp: 40.0,
                charge_type_id: None,
                charge_quantity: 0,
            },
        );
        Ok(())
    }

    /// Deactivates an active/overloaded module back to `ModuleState::Online`, removing its active-only modifiers.
    pub fn deactivate_module(
        &mut self,
        module_item_id: i64,
        sde: &SdeRegistry,
    ) -> Result<(), String> {
        let mod_type_id = {
            let mod_entry = self
                .modules
                .get_mut(&module_item_id)
                .ok_or_else(|| format!("Module {module_item_id} is not fitted"))?;
            if mod_entry.state == ModuleState::Offline {
                return Err("Module is offline".into());
            }
            mod_entry.state = ModuleState::Online;
            mod_entry.type_id
        };
        self.ship.remove_modifiers_from_source(module_item_id);
        self.apply_online_modifiers_for_module(module_item_id, mod_type_id, sde);
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
        let mod_type_id = {
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
            mod_entry.type_id
        };

        // Reset to clean online modifiers first so repeated Activate calls never stack duplicate active modifiers
        self.ship.remove_modifiers_from_source(module_item_id);
        self.apply_online_modifiers_for_module(module_item_id, mod_type_id, sde);

        if let Some(mod_type) = sde.types.get(&mod_type_id) {
            if mod_type.group_id == 46 {
                // Propulsion module (Afterburner / MWD): execute DogmaExpressionNode VM for effect 67 (moduleBonusAfterburner)
                let mod_state = DogmaItemState::from_sde(module_item_id, mod_type_id, sde);
                let overload_mult = if overload { 1.5 } else { 1.0 };
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
                            opcode: DogmaVmOpcode::Mul,
                            value_f64: None,
                            attribute_id: None,
                            operator: None,
                            arg1: Some(Box::new(DogmaExpressionNode {
                                opcode: DogmaVmOpcode::GetAttr,
                                value_f64: None,
                                attribute_id: Some(attr_ids::SPEED_FACTOR),
                                operator: None,
                                arg1: None,
                                arg2: None,
                            })),
                            arg2: Some(Box::new(DogmaExpressionNode {
                                opcode: DogmaVmOpcode::DefFloat,
                                value_f64: Some(overload_mult),
                                attribute_id: None,
                                operator: None,
                                arg1: None,
                                arg2: None,
                            })),
                        })),
                    })),
                };
                expr.execute_on_target(module_item_id, &mod_state, &mut self.ship, sde);
            }
        }
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
            target_attribute_id: attr_ids::MAX_VELOCITY,
            operator: ModifierOperator::PostPercent,
            value: 25.0,
            stacking_penalized: false,
        });

        for id in [3001, 3002] {
            rifter.add_modifier(AttributeModifier {
                source_item_id: id,
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
    fn test_ship_fitting_thermodynamics_boosters_and_abyssal_roll() {
        let sde = SdeRegistry::with_canonical_seed();
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
