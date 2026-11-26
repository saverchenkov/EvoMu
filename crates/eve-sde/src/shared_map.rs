#![forbid(unsafe_code)]

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::borrow::Borrow;
use std::collections::HashMap;
use std::hash::Hash;
use std::ops::{Deref, DerefMut, Index};
use std::sync::Arc;

/// Copy-on-write `Arc`-backed `HashMap` for [`crate::SdeRegistry`] static tables.
///
/// Cloning is $O(1)$ (atomic refcount bump). Read access transparently dereferences to
/// `&HashMap<K, V>`. Mutable access via `DerefMut` copy-on-writes *only* this map when shared,
/// leaving all other tables on a cloned [`crate::SdeRegistry`] shared in place.
#[derive(Debug)]
pub struct SharedMap<K, V>(Arc<HashMap<K, V>>);

impl<K, V> SharedMap<K, V> {
    pub fn new() -> Self {
        Self(Arc::new(HashMap::new()))
    }

    pub fn ptr_eq(this: &Self, other: &Self) -> bool {
        Arc::ptr_eq(&this.0, &other.0)
    }

    pub fn strong_count(this: &Self) -> usize {
        Arc::strong_count(&this.0)
    }

    pub fn as_arc(&self) -> &Arc<HashMap<K, V>> {
        &self.0
    }

    /// Unwraps the inner `HashMap` if uniquely owned, or clones it if shared.
    pub fn into_inner(self) -> HashMap<K, V>
    where
        K: Clone,
        V: Clone,
    {
        Arc::try_unwrap(self.0).unwrap_or_else(|arc| (*arc).clone())
    }

    /// Consuming key iterator (unwraps when unique, clones inner map when shared).
    pub fn into_keys(self) -> std::collections::hash_map::IntoKeys<K, V>
    where
        K: Clone,
        V: Clone,
    {
        self.into_inner().into_keys()
    }

    /// Consuming value iterator (unwraps when unique, clones inner map when shared).
    pub fn into_values(self) -> std::collections::hash_map::IntoValues<K, V>
    where
        K: Clone,
        V: Clone,
    {
        self.into_inner().into_values()
    }

    // Inherent mutating forwarders preserve Rust's two-phase borrows at call sites such as
    // `sde.types.insert(id, build_from(&sde))`, which would otherwise fail E0502 through `DerefMut`.

    pub fn insert(&mut self, k: K, v: V) -> Option<V>
    where
        K: Eq + Hash + Clone,
        V: Clone,
    {
        Arc::make_mut(&mut self.0).insert(k, v)
    }

    pub fn remove<Q>(&mut self, k: &Q) -> Option<V>
    where
        K: Eq + Hash + Borrow<Q> + Clone,
        Q: ?Sized + Eq + Hash,
        V: Clone,
    {
        Arc::make_mut(&mut self.0).remove(k)
    }

    pub fn get_mut<Q>(&mut self, k: &Q) -> Option<&mut V>
    where
        K: Eq + Hash + Borrow<Q> + Clone,
        Q: ?Sized + Eq + Hash,
        V: Clone,
    {
        Arc::make_mut(&mut self.0).get_mut(k)
    }

    pub fn entry(&mut self, key: K) -> std::collections::hash_map::Entry<'_, K, V>
    where
        K: Eq + Hash + Clone,
        V: Clone,
    {
        Arc::make_mut(&mut self.0).entry(key)
    }

    pub fn clear(&mut self)
    where
        K: Clone,
        V: Clone,
    {
        Arc::make_mut(&mut self.0).clear();
    }

    pub fn extend<T: IntoIterator<Item = (K, V)>>(&mut self, iter: T)
    where
        K: Eq + Hash + Clone,
        V: Clone,
    {
        Arc::make_mut(&mut self.0).extend(iter);
    }
}

impl<K, V> Clone for SharedMap<K, V> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl<K, V> Default for SharedMap<K, V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<K, V> PartialEq for SharedMap<K, V>
where
    K: Eq + Hash,
    V: PartialEq,
{
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}

impl<K, V> Eq for SharedMap<K, V>
where
    K: Eq + Hash,
    V: Eq,
{
}

impl<K, V> From<HashMap<K, V>> for SharedMap<K, V> {
    fn from(map: HashMap<K, V>) -> Self {
        Self(Arc::new(map))
    }
}

impl<K, V> From<Arc<HashMap<K, V>>> for SharedMap<K, V> {
    fn from(arc: Arc<HashMap<K, V>>) -> Self {
        Self(arc)
    }
}

impl<K: Eq + Hash, V> FromIterator<(K, V)> for SharedMap<K, V> {
    fn from_iter<T: IntoIterator<Item = (K, V)>>(iter: T) -> Self {
        Self(Arc::new(HashMap::from_iter(iter)))
    }
}

impl<K, V> Deref for SharedMap<K, V> {
    type Target = HashMap<K, V>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<K: Clone, V: Clone> DerefMut for SharedMap<K, V> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        Arc::make_mut(&mut self.0)
    }
}

impl<K, Q, V> Index<&Q> for SharedMap<K, V>
where
    K: Eq + Hash + Borrow<Q>,
    Q: ?Sized + Eq + Hash,
{
    type Output = V;

    fn index(&self, key: &Q) -> &Self::Output {
        &self.0[key]
    }
}

impl<'a, K, V> IntoIterator for &'a SharedMap<K, V> {
    type Item = (&'a K, &'a V);
    type IntoIter = std::collections::hash_map::Iter<'a, K, V>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

impl<'a, K: Clone, V: Clone> IntoIterator for &'a mut SharedMap<K, V> {
    type Item = (&'a K, &'a mut V);
    type IntoIter = std::collections::hash_map::IterMut<'a, K, V>;

    fn into_iter(self) -> Self::IntoIter {
        Arc::make_mut(&mut self.0).iter_mut()
    }
}

impl<K: Clone, V: Clone> IntoIterator for SharedMap<K, V> {
    type Item = (K, V);
    type IntoIter = std::collections::hash_map::IntoIter<K, V>;

    fn into_iter(self) -> Self::IntoIter {
        self.into_inner().into_iter()
    }
}

impl<K, V> Serialize for SharedMap<K, V>
where
    K: Serialize + Eq + Hash,
    V: Serialize,
{
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.as_ref().serialize(serializer)
    }
}

impl<'de, K, V> Deserialize<'de> for SharedMap<K, V>
where
    K: Deserialize<'de> + Eq + Hash,
    V: Deserialize<'de>,
{
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        HashMap::<K, V>::deserialize(deserializer).map(Self::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BlueprintInfo, InvType, NpcSpawnGroupInfo, SdeCache, SdeRegistry, SolarSystem};

    fn sample_type(type_id: u32, name: &str) -> InvType {
        InvType {
            type_id, group_id: 25, category_id: 6, name: name.to_string(),
            mass: 1e6, volume: 25e3, capacity: 120.0, radius: 40.0, base_price: 1e5,
            published: true, portion_size: 1, market_group_id: None, race_id: Some(2),
            faction_id: None, variation_parent_type_id: None, description_id: None,
            icon_id: None, graphic_id: None, sound_id: None, meta_group_id: None,
            meta_level: None, tech_level: Some(1), default_effect_id: None,
            description: String::new(), effect_ids: Vec::new(), default_attributes: HashMap::new(),
        }
    }

    fn sample_blueprint(blueprint_type_id: u32, product_type_id: u32) -> BlueprintInfo {
        BlueprintInfo {
            blueprint_type_id, product_type_id, product_quantity: 1, manufacturing_time_sec: 600,
            max_production_limit: 10, research_me_time_sec: 100, research_te_time_sec: 100,
            copy_time_sec: 100, invention_time_sec: 0, invention_base_probability: 0.0,
            materials: Vec::new(), activities: HashMap::new(),
        }
    }

    fn sample_system(solar_system_id: u32) -> SolarSystem {
        SolarSystem {
            solar_system_id, constellation_id: 20_000_020, region_id: 10_000_002,
            name: "Jita".to_string(), security_status: 0.9, radius: 1.0e12,
            galactic_pos_ly: [0.0; 3], planet_ids: Vec::new(), stations: Vec::new(),
            wormhole_class_id: None,
        }
    }

    fn sample_spawn(spawn_group_id: u32) -> NpcSpawnGroupInfo {
        NpcSpawnGroupInfo {
            spawn_group_id, faction_id: 500_010, ship_class: 1, group_id: 551,
            group_name: "Pirate Frigate".to_string(), npc_type_ids: vec![23_310],
        }
    }

    #[test]
    fn shared_map_clone_shares_arc_and_cow_isolates_only_mutated_table() {
        let mut base = SdeRegistry::default();
        base.types.insert(587, sample_type(587, "Rifter"));
        base.blueprints.insert(688, sample_blueprint(688, 587));
        base.solar_systems.insert(30_000_142, sample_system(30_000_142));
        base.npc_spawn_groups.insert(1, sample_spawn(1));
        assert_eq!(SharedMap::strong_count(&base.types), 1);

        let mut cloned = base.clone();
        assert_eq!(SharedMap::strong_count(&base.types), 2);
        assert!(SharedMap::ptr_eq(&base.types, &cloned.types));
        assert!(SharedMap::ptr_eq(&base.blueprints, &cloned.blueprints));
        assert!(SharedMap::ptr_eq(&base.solar_systems, &cloned.solar_systems));
        assert!(SharedMap::ptr_eq(&base.npc_spawn_groups, &cloned.npc_spawn_groups));

        cloned.npc_spawn_groups.insert(9001, sample_spawn(9001));
        assert!(!SharedMap::ptr_eq(&base.npc_spawn_groups, &cloned.npc_spawn_groups));
        assert!(!base.npc_spawn_groups.contains_key(&9001));
        assert!(cloned.npc_spawn_groups.contains_key(&9001));
        assert!(SharedMap::ptr_eq(&base.types, &cloned.types));
        assert!(SharedMap::ptr_eq(&base.blueprints, &cloned.blueprints));
        assert!(SharedMap::ptr_eq(&base.solar_systems, &cloned.solar_systems));

        let mut arc_sde = Arc::new(base.clone());
        let arc_other = Arc::clone(&arc_sde);
        Arc::make_mut(&mut arc_sde).npc_spawn_groups.insert(9002, sample_spawn(9002));
        assert!(!Arc::ptr_eq(&arc_sde, &arc_other));
        assert!(!SharedMap::ptr_eq(&base.npc_spawn_groups, &arc_sde.npc_spawn_groups));
        assert!(!base.npc_spawn_groups.contains_key(&9002));
        assert!(SharedMap::ptr_eq(&base.types, &arc_sde.types));
        assert!(SharedMap::ptr_eq(&base.blueprints, &arc_sde.blueprints));
        assert!(SharedMap::ptr_eq(&base.solar_systems, &arc_sde.solar_systems));
        assert_eq!(base.types[&587].name, "Rifter");
    }

    #[test]
    fn shared_map_serde_json_wire_format_matches_plain_hashmap() {
        let plain: HashMap<u32, String> = HashMap::from([(1, "alpha".into()), (2, "beta".into())]);
        let mut shared: SharedMap<u32, String> = plain.clone().into_iter().collect();
        assert_eq!(serde_json::to_value(&shared).unwrap(), serde_json::to_value(&plain).unwrap());

        let wire = serde_json::to_string(&shared).expect("serialize SharedMap");
        let round_trip: SharedMap<u32, String> = serde_json::from_str(&wire).expect("deserialize SharedMap");
        assert_eq!(round_trip, shared);
        assert_eq!(*round_trip, plain);
        assert_eq!(shared.as_arc().len(), 2);
        for (_, v) in &mut shared {
            v.make_ascii_uppercase();
        }
        assert_eq!(round_trip.into_values().count(), 2);
    }

    #[test]
    fn sde_clone_shares_all_tables_while_resetting_derived_caches() {
        struct DerivedVal(usize);

        let cache = SdeCache::new();
        let reg_a = cache.with_canonical_seed();
        let val_a = reg_a.get_or_init_derived(|| DerivedVal(reg_a.types.len()));

        let mut reg_b = cache.with_canonical_seed();
        assert!(SharedMap::ptr_eq(&reg_a.types, &reg_b.types));
        assert!(SharedMap::ptr_eq(&reg_a.blueprints, &reg_b.blueprints));
        assert!(SharedMap::ptr_eq(&reg_a.solar_systems, &reg_b.solar_systems));

        let val_b = reg_b.get_or_init_derived(|| DerivedVal(42));
        assert!(!Arc::ptr_eq(&val_a, &val_b));
        assert_eq!(val_b.0, 42);
        assert_eq!(reg_a.get_or_init_derived(|| DerivedVal(0)).0, reg_a.types.len());

        let reg_c = reg_a.clone();
        assert!(SharedMap::ptr_eq(&reg_a.types, &reg_c.types));
        let val_c = reg_c.get_or_init_derived(|| DerivedVal(77));
        assert!(!Arc::ptr_eq(&val_a, &val_c));
        assert_eq!(val_c.0, 77);

        reg_b.npc_spawn_groups.clear();
        assert!(!SharedMap::ptr_eq(&reg_a.npc_spawn_groups, &reg_b.npc_spawn_groups));
        assert!(!reg_a.npc_spawn_groups.is_empty());
        assert!(SharedMap::ptr_eq(&reg_a.types, &reg_b.types));
    }
}
