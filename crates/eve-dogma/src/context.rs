use crate::*;
use eve_sde::{
    AttributeId, CategoryId, DogmaEffectType, EffectId, GroupId, SdeRegistry, TypeId,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

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
            DogmaModifierFunc::ItemModifier | DogmaModifierFunc::ItemModifierWithFalloff => {
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

    /// Applies a single [`EffectModifierSpec`] scaled by range falloff attenuation (`DG-1`):
    ///
    /// $$E(d) = E_0 \cdot 0.5^{\left(\frac{\max(0, d - R_{\text{opt}})}{R_{\text{falloff}}}\right)^2}$$
    pub fn apply_modifier_spec_with_falloff(
        &mut self,
        source_item_id: i64,
        effect_id: Option<EffectId>,
        spec: &EffectModifierSpec,
        distance: f64,
        optimal: f64,
        falloff: f64,
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

        let scale = compute_falloff_attenuation(distance, optimal, falloff);
        let source_snapshot = self.item.clone();
        let mut applied = 0usize;

        match func {
            DogmaModifierFunc::ItemModifier | DogmaModifierFunc::ItemModifierWithFalloff => {
                let target_entity = self.entity_for_domain_mut(domain);
                let before = target_entity.modifiers.len();
                vm_node.execute_on_target_with_effect_and_scale(
                    source_item_id,
                    effect_id,
                    &source_snapshot,
                    target_entity,
                    scale,
                    sde,
                );
                if target_entity.modifiers.len() > before {
                    applied += 1;
                }
            }
            DogmaModifierFunc::LocationModifier
            | DogmaModifierFunc::LocationGroupModifier
            | DogmaModifierFunc::LocationRequiredSkillModifier => {
                let list = self.located_items_for_domain_mut(domain);
                for child in list.iter_mut() {
                    let b = child.modifiers.len();
                    vm_node.execute_on_target_with_effect_and_scale(
                        source_item_id,
                        effect_id,
                        &source_snapshot,
                        child,
                        scale,
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
                    vm_node.execute_on_target_with_effect_and_scale(
                        source_item_id,
                        effect_id,
                        &source_snapshot,
                        child,
                        scale,
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

    /// Applies all [`EffectModifierSpec`] entries on `effect` scaled by distance falloff (`DG-1`).
    pub fn apply_effect_with_range(
        &mut self,
        source_item_id: i64,
        effect: &DogmaEffectType,
        distance: f64,
        optimal: f64,
        falloff: f64,
        sde: &SdeRegistry,
    ) -> usize {
        let mut total = 0;
        for spec in &effect.modifiers {
            total += self.apply_modifier_spec_with_falloff(
                source_item_id,
                Some(effect.effect_id),
                spec,
                distance,
                optimal,
                falloff,
                sde,
            );
        }
        total
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
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
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

/// Computes the standard EVE Online range falloff attenuation factor (`DG-1`):
///
/// $$\text{factor} = \begin{cases} 1.0 & \text{if } d \le R_{\text{opt}} \text{ or } R_{\text{falloff}} \le 0 \\ 0.5^{\left(\frac{d - R_{\text{opt}}}{R_{\text{falloff}}}\right)^2} & \text{if } d > R_{\text{opt}} \end{cases}$$
#[inline]
pub fn compute_falloff_attenuation(distance: f64, optimal: f64, falloff: f64) -> f64 {
    if distance <= optimal || falloff <= 0.0 {
        1.0
    } else {
        let excess = distance - optimal;
        0.5_f64.powf((excess / falloff).powi(2))
    }
}

/// Computes EVE Online's stacking penalty factor for the `n`-th (0-indexed) module modifier (`DG-1`):
///
/// $$S(n) = e^{-(n-1)^2 / 7.1289}$$
#[inline]
pub fn stacking_penalty_factor(index: usize) -> f64 {
    let idx = index as f64;
    (-idx * idx / 7.1289).exp()
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
    /// Dynamic attribute overrides (e.g. Abyssal Mutaplasmid rolls) applied before modifiers (`feat:dogma.mutaplasmids`, `T-12`).
    #[serde(default)]
    pub dynamic_attribute_overrides: HashMap<AttributeId, f64>,
}

/// An active command burst / gang warfare burst buff affecting this ship (`feat:dogma.gang-bursts`, `T-12`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GangBurstEffect {
    #[serde(default)]
    pub source_item_id: i64,
    #[serde(default)]
    pub source_type_id: Option<TypeId>,
    /// Optional SDE dbuff collection ID (from `sde_dbuff_collections`, e.g. 10=Shield Harmonizing, 12=Shield Extension, 13=Armor Energizing, 15=Armor Reinforcement, 20=Skirmish Evasive, 22=Skirmish Rapid).
    #[serde(default)]
    pub dbuff_id: Option<u32>,
    /// Burst bonus value (e.g. `15.0` for +15% HP or `-15.0` for -15% resonance / sig radius).
    #[serde(default)]
    pub value: f64,
    /// Direct attribute modifiers applied by the burst.
    #[serde(default)]
    pub modifiers: Vec<AttributeModifier>,
}

/// Stateful Dogma container for a ship or item entity.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
                // DERIVED_OP9_SPECIAL_SKILL_RELATIONSHIP: Operator 9 (SpecialSkillRelationship /
                // dgmAssSpecialSkillRelationship) is a DERIVED EvoMu extension to CCP's 10 standard
                // operators [-1, 0, 1, 2, 3, 4, 5, 6, 8, 7]. Direct percentage modifier (m.value / 100.0)
                // matching dogma_evaluator_py2.py (lambda v, m: v * (1.0 + m / 100.0)).
                ModifierOperator::SpecialSkillRelationship => {
                    (Some(m.value / 100.0), true)
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

        // Normalize near-zero EWAR resistance and remote impedance values (e.g. Bastion `-99.9999%` / Siege `1e-5` multipliers) to exact `0.0` (`feat:dogma.ewar-resistance-attributes`).
        if matches!(
            attribute_id,
            2045 | 2112 | 2113 | 2114 | 2115 | 2116 | 2135 | 2253 | 6463
        ) && val.abs() <= 1e-4
        {
            val = 0.0;
        }

        val
    }
}

/// A hostile projected EWAR module cycle active against a target ship (`feat:modules.tracking-disruptor`, `DG-5`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectedEwarEffect {
    pub source_module_id: i64,
    pub source_module_type_id: TypeId,
    pub charge_type_id: Option<TypeId>,
    pub distance: f64,
    pub overloaded: bool,
    pub effective_module: DogmaItemState,
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
