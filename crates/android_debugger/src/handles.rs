use anyhow::{Result, anyhow, ensure};
use std::collections::BTreeMap;

/// DAP handles never reuse an ID, including after a resume. A delayed request
/// therefore cannot resolve to a new frame at the same JDWP location.
pub struct Handles<T> {
    next: u64,
    entries: BTreeMap<u64, (u64, T)>,
}

impl<T> Default for Handles<T> {
    fn default() -> Self {
        Self {
            next: 1,
            entries: BTreeMap::new(),
        }
    }
}

impl<T> Handles<T> {
    pub fn insert(&mut self, owner: u64, value: T) -> Result<u64> {
        ensure!(
            self.entries.len() < 100_000,
            "too many suspended inspection handles"
        );
        let id = self.next;
        self.next = self
            .next
            .checked_add(1)
            .ok_or_else(|| anyhow!("inspection handle overflow"))?;
        self.entries.insert(id, (owner, value));
        Ok(id)
    }

    pub fn get(&self, id: u64) -> Result<&T> {
        self.entries
            .get(&id)
            .map(|(_, value)| value)
            .ok_or_else(|| anyhow!("stale or unknown inspection handle {id}"))
    }

    pub fn remove_owner(&mut self, owner: u64) {
        self.entries
            .retain(|_, (entry_owner, _)| *entry_owner != owner);
    }

    pub fn owned_by(&self, owner: u64) -> impl Iterator<Item = &T> {
        self.entries
            .values()
            .filter(move |(entry_owner, _)| *entry_owner == owner)
            .map(|(_, value)| value)
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Behavior adapted from fwcd/kotlin-debug-adapter ObjectPoolTest.testObjectPool
    // at 7f05669b642d21afa46ac7b75307fa5d523a7263 (MIT; attribution in NOTICE).
    #[test]
    fn object_pool_ownership() {
        let mut pool = Handles::default();
        assert_eq!(pool.len(), 0);
        let park = pool.insert(1, "park").unwrap();
        let river = pool.insert(1, "river").unwrap();
        let streets = pool.insert(1, "streets").unwrap();
        let door = pool.insert(2, "door").unwrap();
        let kitchen = pool.insert(2, "kitchen").unwrap();
        let living_room = pool.insert(2, "livingRoom").unwrap();
        assert_eq!(pool.len(), 6);
        assert_eq!(
            pool.owned_by(1)
                .copied()
                .collect::<std::collections::BTreeSet<_>>(),
            ["park", "river", "streets"].into()
        );
        assert_eq!(
            pool.owned_by(2)
                .copied()
                .collect::<std::collections::BTreeSet<_>>(),
            ["door", "kitchen", "livingRoom"].into()
        );
        assert_eq!(*pool.get(park).unwrap(), "park");
        assert_eq!(*pool.get(river).unwrap(), "river");
        assert_eq!(*pool.get(streets).unwrap(), "streets");
        assert_eq!(*pool.get(door).unwrap(), "door");
        assert_eq!(*pool.get(kitchen).unwrap(), "kitchen");
        assert_eq!(*pool.get(living_room).unwrap(), "livingRoom");
        pool.remove_owner(1);
        assert_eq!(pool.len(), 3);
        assert!(pool.get(park).is_err());
        pool.entries.remove(&door);
        assert_eq!(pool.len(), 2);
        assert_eq!(
            pool.owned_by(2)
                .copied()
                .collect::<std::collections::BTreeSet<_>>(),
            ["kitchen", "livingRoom"].into()
        );
        pool.remove_owner(2);
        assert_eq!(pool.len(), 0);
    }

    #[test]
    fn resumed_handles_never_alias_new_frames() {
        let mut pool = Handles::default();
        let old = pool.insert(7, "old").unwrap();
        pool.clear();
        let new = pool.insert(7, "new").unwrap();
        assert_ne!(old, new);
        assert!(pool.get(old).is_err());
        assert_eq!(*pool.get(new).unwrap(), "new");
    }
}
