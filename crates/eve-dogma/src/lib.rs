//! `eve-dogma` — EVE Online Dogma Attribute & Effect Modifier Engine.
//!
//! Implements the exact operator precedence, SDE operation code mapping (`-1..=7`),
//! attribute-aware stacking penalty curve, and passive capacitor/shield regeneration equations.

#![forbid(unsafe_code)]

use eve_sde::{AttributeId, SdeRegistry, TypeId};
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
    /// Final hard override (`dgmAssPostAssignment`, SDE op `7`).
    PostAssign = 8,
}

impl ModifierOperator {
    /// Converts an EVE SDE `operation` integer (`-1..=7`) into a [`ModifierOperator`].
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
            _ => None,
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

/// Stateful Dogma container for a ship or item entity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DogmaItemState {
    pub item_id: i64,
    pub type_id: TypeId,
    pub base_attributes: HashMap<AttributeId, f64>,
    pub modifiers: Vec<AttributeModifier>,
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
        }
    }

    pub fn add_modifier(&mut self, modifier: AttributeModifier) {
        self.modifiers.push(modifier);
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

        // 4. PostMul / PostDiv / PostPercent
        let mut unpenalized_deltas = Vec::new();
        let mut penalized_pos_deltas = Vec::new();
        let mut penalized_neg_deltas = Vec::new();

        for m in &relevant {
            let delta_opt = match m.operator {
                ModifierOperator::PostMul => Some(m.value - 1.0),
                ModifierOperator::PostDiv if m.value != 0.0 => Some((1.0 / m.value) - 1.0),
                ModifierOperator::PostPercent => Some(m.value / 100.0),
                _ => None,
            };
            if let Some(d) = delta_opt {
                let is_penalized = !attr_allows_free_stacking && m.stacking_penalized;
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
    fn test_sde_operator_mapping() {
        assert_eq!(
            ModifierOperator::try_from_sde_op(-1),
            Some(ModifierOperator::PreAssign)
        );
        assert_eq!(
            ModifierOperator::try_from_sde_op(6),
            Some(ModifierOperator::PostPercent)
        );
        assert_eq!(
            ModifierOperator::try_from_sde_op(7),
            Some(ModifierOperator::PostAssign)
        );
        assert_eq!(ModifierOperator::try_from_sde_op(99), None);
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
    fn test_stackable_attribute_exempt_from_penalty() {
        let sde = SdeRegistry::with_canonical_seed();
        let mut rifter = DogmaItemState::from_sde(1001, 587, &sde);

        for id in [4001, 4002] {
            rifter.add_modifier(AttributeModifier {
                source_item_id: id,
                target_attribute_id: attr_ids::SHIELD_CAPACITY,
                operator: ModifierOperator::PostPercent,
                value: 20.0,
                stacking_penalized: true,
            });
        }

        let shield_hp = rifter.get_attribute(attr_ids::SHIELD_CAPACITY, &sde);
        assert!((shield_hp - 648.0).abs() < 1e-6);
    }

    #[test]
    fn test_capacitor_recharge_curve() {
        let cap_after_10s = compute_passive_recharge(62.5, 250.0, 125_000.0, 10_000.0);
        assert!(cap_after_10s > 62.5 && cap_after_10s < 250.0);
    }
}
