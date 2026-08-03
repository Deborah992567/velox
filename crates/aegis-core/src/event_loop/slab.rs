//! A generation-counted slab of I/O objects.
//!
//! Each allocated slot is addressed by a [`Token`] that pairs the slot index
//! with a generation counter. When a slot is freed and later reused, its
//! generation increments, so a stale [`Token`] no longer resolves to the new
//! occupant. This is what makes fd churn safe: a reused descriptor can never
//! be confused with the connection that used it before (see
//! `docs/architecture.md` §3.1).

use crate::platform::Token;

/// One slab slot: the occupant plus its generation guard.
#[derive(Debug)]
struct Slot<T> {
    generation: u32,
    value: Option<T>,
}

/// A dense, generation-counted store indexed by [`Token`].
///
/// Not `Send`-safe by design: one slab belongs to one reactor/thread.
#[derive(Debug)]
pub struct Slab<T> {
    slots: Vec<Slot<T>>,
    free: Vec<u32>,
}

impl<T> Default for Slab<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> Slab<T> {
    /// Create an empty slab.
    pub const fn new() -> Self {
        Self {
            slots: Vec::new(),
            free: Vec::new(),
        }
    }

    /// Allocate a slot for `value` and return its token.
    ///
    /// # Panics
    ///
    /// Panics if the slab outgrows the u32 index space.
    pub fn insert(&mut self, value: T) -> Token {
        if let Some(index) = self.free.pop() {
            let slot = &mut self.slots[usize::try_from(index).expect("slot index fits usize")];
            let generation = slot.generation;
            slot.value = Some(value);
            Token::from_parts(index, generation)
        } else {
            let index = u32::try_from(self.slots.len()).expect("slab exceeds 4 billion slots");
            self.slots.push(Slot {
                generation: 1,
                value: Some(value),
            });
            Token::from_parts(index, 1)
        }
    }

    /// Free the slot at `token`, returning its occupant. Returns `None` for a
    /// stale token or a token that was already removed.
    ///
    /// # Panics
    ///
    /// Panics if the slab outgrows the usize index space.
    pub fn remove(&mut self, token: Token) -> Option<T> {
        let index = usize::try_from(token.index()).expect("slot index fits usize");
        let slot = self.slots.get_mut(index)?;
        if slot.generation != token.generation() {
            return None;
        }
        let value = slot.value.take();
        slot.generation = slot.generation.wrapping_add(1);
        self.free.push(token.index());
        value
    }

    /// Borrow the occupant of `token`.
    ///
    /// # Panics
    ///
    /// Panics if the slab outgrows the usize index space.
    pub fn get(&self, token: Token) -> Option<&T> {
        let index = usize::try_from(token.index()).expect("slot index fits usize");
        let slot = self.slots.get(index)?;
        if slot.generation != token.generation() {
            return None;
        }
        slot.value.as_ref()
    }

    /// Mutably borrow the occupant of `token`.
    ///
    /// # Panics
    ///
    /// Panics if the slab outgrows the usize index space.
    pub fn get_mut(&mut self, token: Token) -> Option<&mut T> {
        let index = usize::try_from(token.index()).expect("slot index fits usize");
        let slot = self.slots.get_mut(index)?;
        if slot.generation != token.generation() {
            return None;
        }
        slot.value.as_mut()
    }

    /// Number of occupied slots.
    pub fn len(&self) -> usize {
        self.slots
            .iter()
            .filter(|slot| slot.value.is_some())
            .count()
    }

    /// Whether no slot is occupied.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Iterate over occupied slots.
    ///
    /// # Panics
    ///
    /// Panics if the slab outgrows the u32 index space.
    pub fn iter(&self) -> impl Iterator<Item = (Token, &T)> {
        self.slots.iter().enumerate().filter_map(|(index, slot)| {
            let token = Token::from_parts(
                u32::try_from(index).expect("slot index fits u32"),
                slot.generation,
            );
            slot.value.as_ref().map(|value| (token, value))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::Slab;

    #[test]
    fn insert_get_and_len() {
        let mut slab = Slab::new();
        assert!(slab.is_empty());
        let a = slab.insert("a");
        let b = slab.insert("b");
        assert_eq!(slab.len(), 2);
        assert_eq!(slab.get(a), Some(&"a"));
        assert_eq!(slab.get(b), Some(&"b"));
    }

    #[test]
    fn remove_frees_and_bumps_generation() {
        let mut slab = Slab::new();
        let a = slab.insert("a");
        assert_eq!(slab.remove(a), Some("a"));
        assert!(slab.remove(a).is_none(), "token is stale after removal");
        assert!(slab.get(a).is_none());
        assert!(slab.is_empty());
        // Slot reuse must mint a fresh generation.
        let b = slab.insert("b");
        assert_eq!(b.index(), a.index(), "slot was reused");
        assert_ne!(b.generation(), a.generation(), "generation was bumped");
        assert!(
            slab.get(a).is_none(),
            "old token must not reach new occupant"
        );
        assert_eq!(slab.get(b), Some(&"b"));
    }

    #[test]
    fn stale_generation_is_rejected() {
        let mut slab = Slab::new();
        let a = slab.insert("a");
        let b = slab.insert("b");
        slab.remove(b);
        slab.remove(a);
        let c = slab.insert("c");
        assert!(slab.get(a).is_none(), "a is stale after removal");
        assert!(slab.get(b).is_none(), "b is stale after removal");
        assert_eq!(slab.get(c), Some(&"c"));
        assert_eq!(slab.len(), 1);
    }

    #[test]
    fn iter_visits_occupied_slots_only() {
        let mut slab = Slab::new();
        let a = slab.insert(1);
        slab.insert(2);
        slab.remove(a);
        let mut seen: Vec<u32> = slab.iter().map(|(_, v)| *v).collect();
        seen.sort_unstable();
        assert_eq!(seen, vec![2]);
    }
}
