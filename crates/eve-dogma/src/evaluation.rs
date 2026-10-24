use crate::*;
use eve_sde::{AttributeId, SdeRegistry, TypeId};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};

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
    /// Active character implants (`type_id` of each implant).
    #[serde(default)]
    pub implants: Vec<TypeId>,
    /// Active combat booster drugs (`type_id` of each booster).
    #[serde(default)]
    pub boosters: Vec<TypeId>,
    /// Rolled side-effect `effect_id`s per active combat booster `type_id` (`feat:attributes.booster-side-effects`).
    /// When `None` for a booster `type_id`, all passive effects on the booster apply; when `Some(eids)`,
    /// side-effect penalty effects only apply if listed in `eids`.
    #[serde(default)]
    pub booster_side_effects: HashMap<TypeId, Vec<u32>>,
    /// Active hostile EWAR modules projected onto this ship (`feat:modules.tracking-disruptor`, `DG-5`).
    #[serde(default)]
    pub projected_ewar: Vec<ProjectedEwarEffect>,
    /// Active command burst / gang warfare burst effects affecting this ship (`feat:dogma.gang-bursts`, `T-12`).
    #[serde(default)]
    pub gang_bursts: Vec<GangBurstEffect>,
    /// Active Tactical Destroyer (`group_id == 1305`) stance (`group_id == 1306`, Defense / Propulsion / Sharpshooter).
    #[serde(default)]
    pub active_stance_type_id: Option<TypeId>,
    /// Timestamp (`ms`) of the most recent Tactical Destroyer stance switch, enforcing `stanceSwitchTime` (`1985`).
    #[serde(default)]
    pub last_stance_switch_ms: Option<u64>,
    /// Owned drones in space or drone bay (`(drone_item_id, drone_type_id)`) evaluated under `ctx.owned_items`.
    #[serde(default)]
    pub drones: Vec<(i64, TypeId)>,
}

impl ShipFittingEngine {
    pub fn from_sde(ship_item_id: i64, ship_type_id: TypeId, sde: &SdeRegistry) -> Self {
        Self {
            ship: DogmaItemState::from_sde(ship_item_id, ship_type_id, sde),
            modules: HashMap::new(),
            rack_heat: [0.0, 0.0, 0.0],
            character_skills: HashMap::new(),
            implants: Vec::new(),
            boosters: Vec::new(),
            booster_side_effects: HashMap::new(),
            projected_ewar: Vec::new(),
            gang_bursts: Vec::new(),
            active_stance_type_id: None,
            last_stance_switch_ms: None,
            drones: Vec::new(),
        }
    }

    pub fn set_character_skills(&mut self, skills: HashMap<TypeId, u8>) {
        self.character_skills = skills;
    }

    pub fn with_character_skills(mut self, skills: HashMap<TypeId, u8>) -> Self {
        self.character_skills = skills;
        self
    }

    pub fn set_implants(&mut self, implants: Vec<TypeId>) {
        self.implants = implants;
    }

    pub fn with_implants(mut self, implants: Vec<TypeId>) -> Self {
        self.implants = implants;
        self
    }

    pub fn set_boosters(&mut self, boosters: Vec<TypeId>) {
        self.boosters = boosters;
    }

    pub fn with_boosters(mut self, boosters: Vec<TypeId>) -> Self {
        self.boosters = boosters;
        self
    }

    /// Registers an owned drone in `self.drones` and recalculates stats (`feat:drones.drone-damage-skills`).
    pub fn add_drone(&mut self, drone_item_id: i64, drone_type_id: TypeId, sde: &SdeRegistry) {
        if let Some(entry) = self.drones.iter_mut().find(|(id, _)| *id == drone_item_id) {
            entry.1 = drone_type_id;
        } else {
            self.drones.push((drone_item_id, drone_type_id));
        }
        self.recalculate_stats(sde);
    }

    /// Removes an owned drone from `self.drones` and recalculates stats.
    pub fn remove_drone(&mut self, drone_item_id: i64, sde: &SdeRegistry) -> bool {
        let before = self.drones.len();
        self.drones.retain(|(id, _)| *id != drone_item_id);
        let removed = self.drones.len() < before;
        if removed {
            self.recalculate_stats(sde);
        }
        removed
    }

    /// Returns the evaluated [`DogmaItemState`] of an owned drone (including character skill, ship hull, and DDA modifiers).
    pub fn drone_state(&self, drone_item_id: i64, sde: &SdeRegistry) -> Option<DogmaItemState> {
        self.evaluated_context(sde)
            .owned_items
            .into_iter()
            .find(|d| d.item_id == drone_item_id)
    }

    /// Returns the evaluated dogma attribute for an owned drone (`feat:drones.drone-damage-skills`).
    pub fn get_drone_effective_attribute(
        &self,
        drone_item_id: i64,
        attr_id: AttributeId,
        sde: &SdeRegistry,
    ) -> f64 {
        if let Some(st) = self.drone_state(drone_item_id, sde) {
            return st.get_attribute(attr_id, sde);
        }
        self.drones
            .iter()
            .find(|(id, _)| *id == drone_item_id)
            .map(|&(id, tid)| DogmaItemState::from_sde(id, tid, sde).get_attribute(attr_id, sde))
            .unwrap_or(0.0)
    }

    /// Builds and evaluates a [`DogmaEffectContext`] for the current ship hull, character skills,
    /// and fitted modules over real V24.01 SDE `modifierInfo` records (`DG-1`, `OF-31`).
    pub fn evaluated_context(&self, sde: &SdeRegistry) -> DogmaEffectContext {
        let mut base_ship = self.ship.clone();
        base_ship.modifiers.clear();

        let hull_group = sde
            .types
            .get(&self.ship.type_id)
            .map(|t| t.group_id)
            .unwrap_or(0);
        if hull_group == 963 {
            let mut hi_mod = 0.0;
            let mut med_mod = 0.0;
            let mut low_mod = 0.0;
            let mut tur_mod = 0.0;
            let mut lch_mod = 0.0;
            for m in self.modules.values() {
                if (125..=132).contains(&m.flag_id) && m.state != ModuleState::Offline {
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
            base_ship.base_attributes.insert(attr_ids::HI_SLOTS, hi_mod);
            base_ship.base_attributes.insert(attr_ids::MED_SLOTS, med_mod);
            base_ship.base_attributes.insert(attr_ids::LOW_SLOTS, low_mod);
            base_ship
                .base_attributes
                .insert(attr_ids::TURRET_SLOTS_LEFT, tur_mod);
            base_ship
                .base_attributes
                .insert(attr_ids::LAUNCHER_SLOTS_LEFT, lch_mod);
        }

        let mut sorted_modules: Vec<FittedModule> = self
            .modules
            .values()
            .filter(|m| m.state != ModuleState::Offline)
            .cloned()
            .collect();
        // Evaluate T3C subsystems (`flag_id` `125..=132`) before standard modules so subsystem
        // `LocationRequiredSkillModifier` / `LocationGroupModifier` bonuses modify fitted modules
        // before active module effects (e.g. MWD/AB `6730`/`6731` or Covert Cloak `5261`) read module attributes.
        sorted_modules.sort_by_key(|m| {
            let is_subsystem = (125..=132).contains(&m.flag_id);
            (!is_subsystem, m.flag_id, m.item_id)
        });

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
        for &(drone_item_id, drone_type_id) in &self.drones {
            ctx.owned_items
                .push(DogmaItemState::from_sde(drone_item_id, drone_type_id, sde));
        }
        for m in &sorted_modules {
            let mut mod_state = DogmaItemState::from_sde(m.item_id, m.type_id, sde);
            for (&attr_id, &val) in &m.dynamic_attribute_overrides {
                mod_state.base_attributes.insert(attr_id, val);
            }
            if let Some(ctid) = m.charge_type_id {
                if let Some(charge_type) = sde.types.get(&ctid) {
                    let charge_state = DogmaItemState::from_sde(m.item_id, ctid, sde);
                    // Propagate missile flight & explosion base attributes onto launcher module state
                    // and register the loaded charge in `ctx.owned_items`.
                    for missile_attr in [37_u16, 281, 653, 654] {
                        if let Some(&val) = charge_type.default_attributes.get(&missile_attr) {
                            mod_state.base_attributes.entry(missile_attr).or_insert(val);
                        }
                    }
                    ctx.owned_items.push(charge_state.clone());
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
                        // Parameterized skill effects (`1730` `droneDmgBonus`, `660..662`/`668` missile damage,
                        // `848` `Cloaking`, `1851` `selfRof`) have empty `modifiers` in `dogmaeffects.fsdbinary`
                        // because `skill_type_id` is the trained skill's own `skill_tid` (`feat:drones.drone-damage-skills`).
                        let param_spec = match eid {
                            1730 => Some(EffectModifierSpec {
                                domain: "charID".to_string(),
                                func: "OwnerRequiredSkillModifier".to_string(),
                                modified_attribute_id: attr_ids::DAMAGE_MULTIPLIER,
                                modifying_attribute_id: 292,
                                operator: 6,
                                group_id: None,
                                skill_type_id: Some(skill_tid),
                                effect_id: None,
                            }),
                            660 => Some(EffectModifierSpec {
                                domain: "charID".to_string(),
                                func: "OwnerRequiredSkillModifier".to_string(),
                                modified_attribute_id: attr_ids::EM_DAMAGE,
                                modifying_attribute_id: 292,
                                operator: 6,
                                group_id: None,
                                skill_type_id: Some(skill_tid),
                                effect_id: None,
                            }),
                            661 => Some(EffectModifierSpec {
                                domain: "charID".to_string(),
                                func: "OwnerRequiredSkillModifier".to_string(),
                                modified_attribute_id: attr_ids::EXPLOSIVE_DAMAGE,
                                modifying_attribute_id: 292,
                                operator: 6,
                                group_id: None,
                                skill_type_id: Some(skill_tid),
                                effect_id: None,
                            }),
                            662 => Some(EffectModifierSpec {
                                domain: "charID".to_string(),
                                func: "OwnerRequiredSkillModifier".to_string(),
                                modified_attribute_id: attr_ids::THERMAL_DAMAGE,
                                modifying_attribute_id: 292,
                                operator: 6,
                                group_id: None,
                                skill_type_id: Some(skill_tid),
                                effect_id: None,
                            }),
                            668 => Some(EffectModifierSpec {
                                domain: "charID".to_string(),
                                func: "OwnerRequiredSkillModifier".to_string(),
                                modified_attribute_id: attr_ids::KINETIC_DAMAGE,
                                modifying_attribute_id: 292,
                                operator: 6,
                                group_id: None,
                                skill_type_id: Some(skill_tid),
                                effect_id: None,
                            }),
                            848 => Some(EffectModifierSpec {
                                domain: "shipID".to_string(),
                                func: "LocationRequiredSkillModifier".to_string(),
                                modified_attribute_id: 560,
                                modifying_attribute_id: 619,
                                operator: 6,
                                group_id: None,
                                skill_type_id: Some(skill_tid),
                                effect_id: None,
                            }),
                            1851 => Some(EffectModifierSpec {
                                domain: "shipID".to_string(),
                                func: "LocationRequiredSkillModifier".to_string(),
                                modified_attribute_id: attr_ids::SPEED,
                                modifying_attribute_id: 293,
                                operator: 6,
                                group_id: None,
                                skill_type_id: Some(skill_tid),
                                effect_id: None,
                            }),
                            _ => None,
                        };
                        if let Some(ref spec) = param_spec {
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

        // 1b. Evaluate active character implants (`category_id == 20` or `group_id == 300`, passive effects `effect_category == 0`)
        //     in 3 passes so pirate implant set multipliers (`PreMul` on `charID` via `LocationGroupModifier` grp 300 or
        //     `LocationRequiredSkillModifier` skill 3411) compound across all plugged-in implants before each implant
        //     applies its `shipID` / `charID` bonus (`feat:attributes.implant-sets`).
        let mut active_implants: Vec<(usize, i64, TypeId)> = Vec::new();
        for (idx, &imp_tid) in self.implants.iter().enumerate() {
            let Some(imp_type) = sde.types.get(&imp_tid) else {
                continue;
            };
            let imp_item_id = -100_000 - (idx as i64);
            let mut imp_state = DogmaItemState::from_sde(imp_item_id, imp_tid, sde);
            for &eid in &imp_type.effect_ids {
                if let Some(effect) = sde.effects.get(&eid) {
                    if effect.effect_category == 0 {
                        for spec in &effect.modifiers {
                            if spec.domain == "itemID" {
                                if let Some(vm) = compile_modifier_spec_to_vm(spec, false) {
                                    let snap = imp_state.clone();
                                    vm.execute_on_target_with_effect(
                                        imp_item_id,
                                        Some(eid),
                                        &snap,
                                        &mut imp_state,
                                        sde,
                                    );
                                }
                            }
                        }
                    }
                }
            }
            let char_loc_idx = ctx.char_located_items.len();
            ctx.char_located_items.push(imp_state);
            active_implants.push((char_loc_idx, imp_item_id, imp_tid));
        }

        // Pass 2: Apply `charID`-domain modifiers (including pirate implant set `PreMul` multipliers when >= 2 implants are active).
        let multi_implant_set = active_implants.len() > 1;
        for &(char_loc_idx, imp_item_id, imp_tid) in &active_implants {
            let Some(imp_type) = sde.types.get(&imp_tid) else {
                continue;
            };
            ctx.item = ctx.char_located_items[char_loc_idx].clone();
            for &eid in &imp_type.effect_ids {
                if let Some(effect) = sde.effects.get(&eid) {
                    if effect.effect_category == 0 {
                        for spec in &effect.modifiers {
                            if spec.domain == "charID" {
                                let is_set_multiplier = spec.operator == 0
                                    && matches!(
                                        spec.func.as_str(),
                                        "LocationGroupModifier" | "LocationRequiredSkillModifier"
                                    );
                                if !is_set_multiplier || multi_implant_set {
                                    ctx.apply_modifier_spec_with_effect(
                                        imp_item_id,
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
        }

        // Pass 3: Using each implant's set-compounded state in `ctx.char_located_items`, apply `shipID` (and non-`charID`/`itemID`) modifiers.
        for &(char_loc_idx, imp_item_id, imp_tid) in &active_implants {
            let Some(imp_type) = sde.types.get(&imp_tid) else {
                continue;
            };
            ctx.item = ctx.char_located_items[char_loc_idx].clone();
            for &eid in &imp_type.effect_ids {
                if let Some(effect) = sde.effects.get(&eid) {
                    if effect.effect_category == 0 {
                        for spec in &effect.modifiers {
                            if spec.domain != "itemID" && spec.domain != "charID" {
                                ctx.apply_modifier_spec_with_effect(
                                    imp_item_id,
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

        // 1c. Evaluate active combat booster drugs (`group_id == 303`, passive effects `effect_category == 0`).
        // Derive side-effect penalty attribute IDs from SDE skill `25538` (`Neurotoxin Control`).
        let penalty_attrs: std::collections::HashSet<u16> = sde
            .types
            .get(&25538)
            .map(|t| {
                t.effect_ids
                    .iter()
                    .filter_map(|eid| sde.effects.get(eid))
                    .flat_map(|eff| eff.modifiers.iter())
                    .filter(|s| s.domain == "charID" && s.group_id == Some(303))
                    .map(|s| s.modified_attribute_id)
                    .collect()
            })
            .unwrap_or_default();
        for (idx, &b_tid) in self.boosters.iter().enumerate() {
            let Some(b_type) = sde.types.get(&b_tid) else {
                continue;
            };
            let b_item_id = -200_000 - (idx as i64);
            let mut b_state = DogmaItemState::from_sde(b_item_id, b_tid, sde);
            for &eid in &b_type.effect_ids {
                if let Some(effect) = sde.effects.get(&eid) {
                    if effect.effect_category == 0 {
                        for spec in &effect.modifiers {
                            if spec.domain == "itemID" {
                                if let Some(vm) = compile_modifier_spec_to_vm(spec, false) {
                                    let snap = b_state.clone();
                                    vm.execute_on_target_with_effect(
                                        b_item_id,
                                        Some(eid),
                                        &snap,
                                        &mut b_state,
                                        sde,
                                    );
                                }
                            }
                        }
                    }
                }
            }
            // Apply trained character skills (`charID` `LocationGroupModifier` targeting booster group `303`, e.g. `25538 Neurotoxin Control`).
            for (&skill_tid, &trained_lvl) in &self.character_skills {
                if trained_lvl == 0 {
                    continue;
                }
                let Some(skill_type) = sde.types.get(&skill_tid) else {
                    continue;
                };
                let skill_item_id = -50_000 - (skill_tid as i64);
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
                for &eid in &skill_type.effect_ids {
                    if eid == 132 {
                        continue;
                    }
                    if let Some(effect) = sde.effects.get(&eid) {
                        if effect.effect_category == 0 {
                            for spec in &effect.modifiers {
                                if spec.domain == "charID"
                                    && spec.func == "LocationGroupModifier"
                                    && spec.group_id == Some(b_type.group_id)
                                {
                                    if let Some(vm) = compile_modifier_spec_to_vm(spec, false) {
                                        vm.execute_on_target_with_effect(
                                            skill_item_id,
                                            Some(eid),
                                            &skill_state,
                                            &mut b_state,
                                            sde,
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
            }
            let rolled_side_effects = self.booster_side_effects.get(&b_tid);
            ctx.item = b_state.clone();
            ctx.char_located_items.push(b_state);
            for &eid in &b_type.effect_ids {
                if let Some(effect) = sde.effects.get(&eid) {
                    if effect.effect_category == 0 {
                        // DERIVED_OP9_SPECIAL_SKILL_RELATIONSHIP: Operator 9 is aligned with context.rs
                        // and dogma_evaluator_py2.py to evaluate percentage scaling v * (1.0 + m / 100.0).
                        let is_side_effect = effect
                            .modifiers
                            .iter()
                            .any(|s| penalty_attrs.contains(&s.modifying_attribute_id));
                        if is_side_effect {
                            if let Some(rolled) = rolled_side_effects {
                                if !rolled.contains(&u32::from(eid)) {
                                    continue;
                                }
                            }
                        }
                        for spec in &effect.modifiers {
                            if spec.domain != "itemID" {
                                ctx.apply_modifier_spec_with_effect(
                                    b_item_id,
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

        // 2b. Evaluate active Tactical Destroyer stance (`group_id == 1306`, passive effects `effect_category == 0`) (`feat:ships.ship-stance`).
        if let Some(stance_tid) = self.active_stance_type_id {
            if let Some(stance_type) = sde.types.get(&stance_tid) {
                let stance_item_id = -300_000_i64;
                let stance_state = DogmaItemState::from_sde(stance_item_id, stance_tid, sde);
                ctx.item = stance_state;
                for &eid in &stance_type.effect_ids {
                    if let Some(effect) = sde.effects.get(&eid) {
                        if effect.effect_category == 0 {
                            ctx.apply_effect(stance_item_id, effect, sde);
                        }
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
            let is_active = matches!(m.state, ModuleState::Active | ModuleState::Overloaded);
            // First apply self-modifiers (`domain == "itemID"`, e.g. `5261` `CovertCloakCPUAddition` on Covert Ops Cloaking Device II)
            // so the module's effective CPU/PG/attributes reflect any subsystem or skill modifications before fitting load is added.
            for &eid in &mod_type.effect_ids {
                if let Some(effect) = sde.effects.get(&eid) {
                    let should_apply = match effect.effect_category {
                        0 | 4 => true,
                        1..=3 => is_active,
                        _ => false,
                    };
                    if should_apply {
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
                            if spec.domain == "itemID" {
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
                    // Active Siege (`6582`) and Bastion (`6658`) grant full EWAR and remote assistance/repair immunity (`feat:dogma.ewar-resistance-attributes`).
                    if is_active && matches!(eid, 6582 | 6658) {
                        for resist_attr in [2112_u16, 2113, 2114, 2115, 2116, 2135] {
                            ctx.ship.add_modifier(AttributeModifier {
                                source_item_id: m.item_id,
                                effect_id: Some(eid),
                                target_attribute_id: resist_attr,
                                operator: ModifierOperator::PostPercent,
                                value: -100.0,
                                stacking_penalized: false,
                            });
                        }
                    }
                    // Engine propulsion effects (`6730` MWD, `6731` Afterburner) have empty `modifiers` in `dogmaeffects.fsdbinary`
                    // and read the module's effective `SPEED_FACTOR` (already modified by `Acceleration Control` `1176`, T3D/T3C propulsion bonuses, and `overloadSelfSpeedBonus` `3175`)
                    if is_active && matches!(eid, 6730 | 6731) {
                        let mass_attr_id = if mod_type
                            .default_attributes
                            .contains_key(&attr_ids::MASS_ADDITION)
                        {
                            attr_ids::MASS_ADDITION
                        } else {
                            attr_ids::MASS
                        };
                        let mut nodes = vec![
                            DogmaExpressionNode {
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
                            },
                            DogmaExpressionNode {
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
                            },
                        ];
                        if eid == 6730 {
                            let sig_bonus_attr = if mod_type.default_attributes.contains_key(&554) {
                                554
                            } else if mod_type.default_attributes.contains_key(&986) {
                                986
                            } else {
                                0
                            };
                            if sig_bonus_attr != 0 {
                                nodes.push(DogmaExpressionNode {
                                    opcode: DogmaVmOpcode::AddItmMod,
                                    value_f64: None,
                                    attribute_id: Some(attr_ids::SIGNATURE_RADIUS),
                                    operator: Some(ModifierOperator::PostPercent),
                                    arg1: None,
                                    arg2: Some(Box::new(DogmaExpressionNode {
                                        opcode: DogmaVmOpcode::GetAttr,
                                        value_f64: None,
                                        attribute_id: Some(sig_bonus_attr),
                                        operator: None,
                                        arg1: None,
                                        arg2: None,
                                    })),
                                });
                            }
                        }
                        for node in nodes {
                            node.execute_on_target_with_effect(
                                m.item_id,
                                Some(eid),
                                &mod_state,
                                &mut ctx.ship,
                                sde,
                            );
                        }
                    }
                }
            }
            ctx.ship_located_items[idx] = ctx.item.clone();
        }

        // 5. Evaluate active hostile EWAR modules projected onto this ship (`feat:modules.tracking-disruptor`, `feat:dogma.ewar-resistance-attributes`, `DG-5`).
        let weapon_disruption_resist = ctx.ship.get_attribute(2113, sde).clamp(0.0, 1.0);
        let sensor_dampener_resist = ctx.ship.get_attribute(2112, sde).clamp(0.0, 1.0);
        let target_painter_resist = ctx.ship.get_attribute(2114, sde).clamp(0.0, 1.0);
        let stasis_webifier_resist = ctx.ship.get_attribute(2115, sde).clamp(0.0, 1.0);

        for proj in &self.projected_ewar {
            let optimal = proj.effective_module.get_attribute(attr_ids::MAX_RANGE, sde);
            let falloff = get_module_falloff(&proj.effective_module, sde);
            let att = compute_falloff_attenuation(proj.distance, optimal, falloff);
            let proj_type = sde.types.get(&proj.source_module_type_id);

            let is_tracking_disruptor = proj_type
                .is_some_and(|t| t.effect_ids.contains(&6424))
                || proj.effective_module.base_attributes.contains_key(&767);
            if is_tracking_disruptor {
                let eff_scale = att * weapon_disruption_resist;
                let opt_bonus = proj.effective_module.get_attribute(351, sde) * eff_scale;
                let fall_bonus = proj.effective_module.get_attribute(349, sde) * eff_scale;
                let track_bonus = proj.effective_module.get_attribute(767, sde) * eff_scale;
                for (target_attr_id, bonus_pct) in [
                    (attr_ids::MAX_RANGE, opt_bonus),
                    (attr_ids::FALLOFF, fall_bonus),
                    (attr_ids::TRACKING_SPEED, track_bonus),
                ] {
                    if bonus_pct.abs() > 1e-9 {
                        let m = AttributeModifier {
                            source_item_id: proj.source_module_id,
                            effect_id: Some(6424),
                            target_attribute_id: target_attr_id,
                            operator: ModifierOperator::PostPercent,
                            value: bonus_pct,
                            stacking_penalized: true,
                        };
                        ctx.ship.modifiers.push(m);
                        for loc_item in &mut ctx.ship_located_items {
                            loc_item.modifiers.push(m);
                        }
                    }
                }
            }

            let is_guidance_disruptor = proj_type
                .is_some_and(|t| t.effect_ids.contains(&6423))
                || proj.effective_module.base_attributes.contains_key(&848);
            if is_guidance_disruptor {
                let eff_scale = att * weapon_disruption_resist;
                let vel_bonus = proj.effective_module.get_attribute(547, sde) * eff_scale;
                let delay_bonus = proj.effective_module.get_attribute(596, sde) * eff_scale;
                let aoe_vel_bonus = proj.effective_module.get_attribute(847, sde) * eff_scale;
                let aoe_cloud_bonus = proj.effective_module.get_attribute(848, sde) * eff_scale;
                for (target_attr_id, bonus_pct) in [
                    (attr_ids::MAX_VELOCITY, vel_bonus),
                    (281u16, delay_bonus),
                    (653u16, aoe_vel_bonus),
                    (654u16, aoe_cloud_bonus),
                ] {
                    if bonus_pct.abs() > 1e-9 {
                        let m = AttributeModifier {
                            source_item_id: proj.source_module_id,
                            effect_id: Some(6423),
                            target_attribute_id: target_attr_id,
                            operator: ModifierOperator::PostPercent,
                            value: bonus_pct,
                            stacking_penalized: true,
                        };
                        for loc_item in &mut ctx.ship_located_items {
                            loc_item.modifiers.push(m);
                        }
                        for owned_item in &mut ctx.owned_items {
                            owned_item.modifiers.push(m);
                        }
                    }
                }
            }

            let is_sensor_dampener = proj_type.is_some_and(|t| t.group_id == 208)
                || proj.effective_module.base_attributes.contains_key(&566);
            if is_sensor_dampener {
                let eff_scale = att * sensor_dampener_resist;
                let scan_res_bonus = proj.effective_module.get_attribute(566, sde) * eff_scale;
                let range_bonus = proj.effective_module.get_attribute(309, sde) * eff_scale;
                for (target_attr_id, bonus_pct) in [
                    (attr_ids::SCAN_RESOLUTION, scan_res_bonus),
                    (attr_ids::MAX_TARGET_RANGE, range_bonus),
                ] {
                    if bonus_pct.abs() > 1e-9 {
                        ctx.ship.modifiers.push(AttributeModifier {
                            source_item_id: proj.source_module_id,
                            effect_id: None,
                            target_attribute_id: target_attr_id,
                            operator: ModifierOperator::PostPercent,
                            value: bonus_pct,
                            stacking_penalized: true,
                        });
                    }
                }
            }

            let is_target_painter = proj_type.is_some_and(|t| t.group_id == 379);
            if is_target_painter {
                let sig_bonus =
                    proj.effective_module.get_attribute(554, sde) * att * target_painter_resist;
                if sig_bonus.abs() > 1e-9 {
                    ctx.ship.modifiers.push(AttributeModifier {
                        source_item_id: proj.source_module_id,
                        effect_id: None,
                        target_attribute_id: attr_ids::SIGNATURE_RADIUS,
                        operator: ModifierOperator::PostPercent,
                        value: sig_bonus,
                        stacking_penalized: true,
                    });
                }
            }

            let is_stasis_webifier = proj_type.is_some_and(|t| t.group_id == 65);
            if is_stasis_webifier {
                let speed_bonus =
                    proj.effective_module.get_attribute(20, sde) * att * stasis_webifier_resist;
                if speed_bonus.abs() > 1e-9 {
                    ctx.ship.modifiers.push(AttributeModifier {
                        source_item_id: proj.source_module_id,
                        effect_id: None,
                        target_attribute_id: attr_ids::MAX_VELOCITY,
                        operator: ModifierOperator::PostPercent,
                        value: speed_bonus,
                        stacking_penalized: true,
                    });
                }
            }
        }

        // 6. Evaluate active command burst / gang warfare burst effects (`feat:dogma.gang-bursts`, `T-12`).
        let mut bursts_by_dbuff: BTreeMap<u32, (i64, Vec<f64>)> = BTreeMap::new();
        for burst in &self.gang_bursts {
            if let Some(dbuff_id) = burst.dbuff_id {
                bursts_by_dbuff
                    .entry(dbuff_id)
                    .or_insert_with(|| (burst.source_item_id, Vec::new()))
                    .1
                    .push(burst.value);
            }
        }

        for (&dbuff_id, &(source_item_id, ref vals)) in &bursts_by_dbuff {
            if let Some(dbuff) = sde.dbuff_collections.get(&dbuff_id) {
                let agg_value = aggregate_dbuff_values(&dbuff.aggregate_mode, vals);
                let op = match dbuff.operation.as_str() {
                    "PostPercent" => ModifierOperator::PostPercent,
                    "PostMul" => ModifierOperator::PostMul,
                    "ModAdd" => ModifierOperator::ModAdd,
                    "PostAssignment" => ModifierOperator::PostAssign,
                    _ => ModifierOperator::PostPercent,
                };
                for &aid in &dbuff.item_modifiers {
                    let stacking_penalized = sde.attributes.get(&aid).is_some_and(|a| !a.stackable);
                    ctx.ship.modifiers.push(AttributeModifier {
                        source_item_id,
                        effect_id: None,
                        target_attribute_id: aid,
                        operator: op,
                        value: agg_value,
                        stacking_penalized,
                    });
                }
                for &aid in &dbuff.location_modifiers {
                    let stacking_penalized = sde.attributes.get(&aid).is_some_and(|a| !a.stackable);
                    for loc_item in &mut ctx.ship_located_items {
                        loc_item.modifiers.push(AttributeModifier {
                            source_item_id,
                            effect_id: None,
                            target_attribute_id: aid,
                            operator: op,
                            value: agg_value,
                            stacking_penalized,
                        });
                    }
                }
                for grp_mod in &dbuff.location_group_modifiers {
                    let stacking_penalized = sde.attributes.get(&grp_mod.attribute_id).is_some_and(|a| !a.stackable);
                    for loc_item in &mut ctx.ship_located_items {
                        let grp_matches = loc_item.group_id == Some(grp_mod.group_id)
                            || sde.types.get(&loc_item.type_id).map(|t| t.group_id) == Some(grp_mod.group_id);
                        if grp_matches {
                            loc_item.modifiers.push(AttributeModifier {
                                source_item_id,
                                effect_id: None,
                                target_attribute_id: grp_mod.attribute_id,
                                operator: op,
                                value: agg_value,
                                stacking_penalized,
                            });
                        }
                    }
                }
                for srq_mod in &dbuff.location_required_skill_modifiers {
                    let stacking_penalized = sde.attributes.get(&srq_mod.attribute_id).is_some_and(|a| !a.stackable);
                    for loc_item in &mut ctx.ship_located_items {
                        if item_requires_skill(loc_item, srq_mod.skill_id, sde) {
                            loc_item.modifiers.push(AttributeModifier {
                                source_item_id,
                                effect_id: None,
                                target_attribute_id: srq_mod.attribute_id,
                                operator: op,
                                value: agg_value,
                                stacking_penalized,
                            });
                        }
                    }
                }
            }
        }
        for burst in &self.gang_bursts {
            for m in &burst.modifiers {
                ctx.ship.modifiers.push(*m);
            }
        }

        ctx
    }

    /// Inserts or updates a hostile projected EWAR effect on this ship and recalculates stats.
    pub fn upsert_projected_ewar(&mut self, effect: ProjectedEwarEffect, sde: &SdeRegistry) {
        if let Some(existing) = self
            .projected_ewar
            .iter_mut()
            .find(|e| e.source_module_id == effect.source_module_id)
        {
            *existing = effect;
        } else {
            self.projected_ewar.push(effect);
        }
        self.recalculate_stats(sde);
    }

    /// Removes a hostile projected EWAR effect by `source_module_id` and recalculates stats.
    pub fn remove_projected_ewar(&mut self, source_module_id: i64, sde: &SdeRegistry) -> bool {
        let before = self.projected_ewar.len();
        self.projected_ewar
            .retain(|e| e.source_module_id != source_module_id);
        let removed = self.projected_ewar.len() < before;
        if removed {
            self.recalculate_stats(sde);
        }
        removed
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

    /// Returns the evaluated dogma attribute for a fitted module after character skills, hull bonuses, and modifiers (`DG-1`, `MN-01`).
    pub fn get_module_effective_attribute(&self, module_item_id: i64, attr_id: AttributeId, sde: &SdeRegistry) -> f64 {
        if let Some(st) = self.module_state(module_item_id, sde) {
            return st.get_attribute(attr_id, sde);
        }
        self.modules
            .get(&module_item_id)
            .map(|m| DogmaItemState::from_sde(module_item_id, m.type_id, sde).get_attribute(attr_id, sde))
            .unwrap_or(0.0)
    }

    /// Returns the evaluated dogma attribute for a charge loaded in `launcher_module_id` (such as a missile affected by skills and Guidance Disruptors).
    pub fn get_charge_effective_attribute(
        &self,
        launcher_module_id: i64,
        attr_id: AttributeId,
        sde: &SdeRegistry,
    ) -> f64 {
        let ctx = self.evaluated_context(sde);
        if let Some(ch) = ctx.owned_items.iter().find(|c| c.item_id == launcher_module_id) {
            let mut combined = ch.clone();
            if let Some(mod_state) = ctx
                .ship_located_items
                .iter()
                .find(|m| m.item_id == launcher_module_id)
            {
                for m in &mod_state.modifiers {
                    if m.target_attribute_id == attr_id && !combined.modifiers.contains(m) {
                        combined.modifiers.push(*m);
                    }
                }
            }
            let val = combined.get_attribute(attr_id, sde);
            if val != 0.0 || combined.base_attributes.contains_key(&attr_id) {
                return val;
            }
        }
        self.get_module_effective_attribute(launcher_module_id, attr_id, sde)
    }

    /// Returns the effective evaluated [`DogmaItemState`] of a module (fitted or hypothetical),
    /// including character skill bonuses, overload bonuses, and any loaded charge/script modifiers (`DG-5`, `DG-6`).
    pub fn build_effective_module_state(
        &self,
        module_item_id: i64,
        module_type_id: TypeId,
        overloaded: bool,
        sde: &SdeRegistry,
    ) -> DogmaItemState {
        if overloaded
            && self
                .modules
                .get(&module_item_id)
                .is_some_and(|m| m.state != ModuleState::Overloaded)
        {
            let mut tmp = self.clone();
            if let Some(m) = tmp.modules.get_mut(&module_item_id) {
                m.state = ModuleState::Overloaded;
            }
            if let Some(state) = tmp.module_state(module_item_id, sde) {
                return state;
            }
        }
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
}
