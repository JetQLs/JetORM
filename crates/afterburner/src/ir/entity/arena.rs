use std::marker::PhantomData;

use super::id::ArenaId;

#[derive(Clone, Debug)]
struct Slot<T> {
    // Incremented before a vacant slot is offered for reuse. The generation is
    // stored beside the payload so validation remains one indexed lookup.
    generation: u32,
    value: Option<T>,
}

#[derive(Clone, Debug)]
pub(crate) struct Arena<I, T> {
    slots: Vec<Slot<T>>,
    // Vacant reusable slots. A slot whose generation reaches u32::MAX is retired
    // instead of wrapping and making its oldest handle valid again.
    free: Vec<u32>,
    marker: PhantomData<fn() -> I>,
}

impl<I, T> Arena<I, T>
where
    I: ArenaId,
{
    pub(crate) const fn new() -> Self {
        Self {
            slots: Vec::new(),
            free: Vec::new(),
            marker: PhantomData,
        }
    }

    pub(crate) fn insert(&mut self, value: T) -> I {
        // Reuse keeps arenas dense during repeated rewrite cycles. Generation
        // checks make this safe without updating unrelated handles.
        if let Some(slot_index) = self.free.pop() {
            let slot = &mut self.slots[slot_index as usize];
            debug_assert!(slot.value.is_none());
            slot.value = Some(value);
            return I::from_parts(slot_index, slot.generation);
        }

        let slot_index = u32::try_from(self.slots.len()).expect("IR arena exhausted u32 ids");
        self.slots.push(Slot {
            generation: 0,
            value: Some(value),
        });
        I::from_parts(slot_index, 0)
    }

    pub(crate) fn get(&self, id: I) -> Option<&T> {
        let (slot_index, generation) = id.parts();
        let slot = self.slots.get(slot_index as usize)?;
        (slot.generation == generation)
            .then_some(slot.value.as_ref())
            .flatten()
    }

    pub(crate) fn get_mut(&mut self, id: I) -> Option<&mut T> {
        let (slot_index, generation) = id.parts();
        let slot = self.slots.get_mut(slot_index as usize)?;
        (slot.generation == generation)
            .then_some(slot.value.as_mut())
            .flatten()
    }

    pub(crate) fn remove(&mut self, id: I) -> Option<T> {
        let (slot_index, generation) = id.parts();
        let slot = self.slots.get_mut(slot_index as usize)?;
        if slot.generation != generation {
            return None;
        }
        let value = slot.value.take()?;
        // checked_add permanently retires the practically unreachable terminal
        // generation rather than allowing stale-handle aliasing through wraparound.
        if let Some(next_generation) = slot.generation.checked_add(1) {
            slot.generation = next_generation;
            self.free.push(slot_index);
        }
        Some(value)
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = (I, &T)> {
        self.slots.iter().enumerate().filter_map(|(index, slot)| {
            slot.value
                .as_ref()
                .map(|value| (I::from_parts(index as u32, slot.generation), value))
        })
    }
}
