//! Where the fold's cached block positions live, grouped by their turn.
//!
//! The fold caches, for every item / approval / question / todo / goal, the
//! `(turn, block)` position of the block it renders as. Two things move those
//! positions: an out-of-order [`crate::fold`] insert, which shifts every later
//! block of **one** turn down, and a `TurnRemoved`, which shifts every later
//! **turn** down.
//!
//! Both used to be transcript-wide scans over every cached position (finding
//! `client-adapter-8`). They are not any more:
//!
//! * a [`Slot`] names its turn by a [`TurnKey`] — minted once when the turn is
//!   appended, never reused — so a removed turn costs one pass over the turns
//!   and none over the slots;
//! * [`Slots`] keeps a per-turn index of the positions it holds, so a shift
//!   touches that turn's slots and nothing else;
//! * [`Slots::drop_turn`] takes a removed turn's cached positions out instead
//!   of leaving them behind under a stale index (finding `client-adapter-6`).
//!
//! Every field is private and every write goes through a method, because the
//! per-turn index is only correct if it is impossible to move a slot without
//! telling it.

use std::collections::HashMap;

/// A stable handle for one turn of the folded session.
///
/// Minted when the turn is appended and never reused, so a handle that names a
/// removed turn resolves to nothing rather than to whichever turn slid into
/// its index.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct TurnKey(pub(crate) u32);

/// Where a Muse item's rendering lives in the folded session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Slot {
    /// The turn that hosts the block.
    pub(crate) turn: TurnKey,
    /// Its index among that turn's blocks.
    pub(crate) block: usize,
}

/// Which cached position one per-turn index entry names.
#[derive(Clone, Debug, PartialEq, Eq)]
enum SlotRef {
    Item(String),
    Approval(String),
    /// One question of a `userInput/requested`, by request id and position.
    Input(String, usize),
    Todo,
    Goal,
}

/// Every cached block position the fold holds for one session.
#[derive(Default)]
pub(crate) struct Slots {
    items: HashMap<String, Slot>,
    approvals: HashMap<String, Slot>,
    inputs: HashMap<String, Vec<Slot>>,
    todo: Option<Slot>,
    goal: Option<Slot>,
    /// Turn → the positions it hosts. The whole point of the module: a shift
    /// or a removal reads one bucket instead of every map.
    by_turn: HashMap<TurnKey, Vec<SlotRef>>,
}

impl Slots {
    /// The block an item renders as, if the fold has drawn it.
    pub(crate) fn item(&self, item_id: &str) -> Option<Slot> {
        self.items.get(item_id).copied()
    }

    /// File (or move) an item's block position.
    pub(crate) fn set_item(&mut self, item_id: &str, slot: Slot) {
        let previous = self.items.insert(item_id.to_owned(), slot);
        self.relink(previous, slot, || SlotRef::Item(item_id.to_owned()));
    }

    /// The block an approval card sits in.
    pub(crate) fn approval(&self, approval_id: &str) -> Option<Slot> {
        self.approvals.get(approval_id).copied()
    }

    /// File an approval card's position.
    pub(crate) fn set_approval(&mut self, approval_id: &str, slot: Slot) {
        let previous = self.approvals.insert(approval_id.to_owned(), slot);
        self.relink(previous, slot, || SlotRef::Approval(approval_id.to_owned()));
    }

    /// The question cards one `userInput/requested` drew, in question order.
    pub(crate) fn inputs(&self, user_input_id: &str) -> Option<&[Slot]> {
        self.inputs.get(user_input_id).map(Vec::as_slice)
    }

    /// File the question cards one `userInput/requested` drew.
    pub(crate) fn set_inputs(&mut self, user_input_id: &str, slots: Vec<Slot>) {
        if let Some(previous) = self.inputs.remove(user_input_id) {
            for (index, slot) in previous.into_iter().enumerate() {
                self.unlink(slot.turn, &SlotRef::Input(user_input_id.to_owned(), index));
            }
        }
        for (index, slot) in slots.iter().enumerate() {
            self.link(slot.turn, SlotRef::Input(user_input_id.to_owned(), index));
        }
        self.inputs.insert(user_input_id.to_owned(), slots);
    }

    /// The session's todo card, once one exists.
    pub(crate) fn todo(&self) -> Option<Slot> {
        self.todo
    }

    /// File or clear the session's todo card.
    pub(crate) fn set_todo(&mut self, slot: Option<Slot>) {
        let previous = self.todo;
        self.todo = slot;
        self.reseat(previous, slot, SlotRef::Todo);
    }

    /// The session's goal card, once one exists.
    pub(crate) fn goal(&self) -> Option<Slot> {
        self.goal
    }

    /// File or clear the session's goal card.
    pub(crate) fn set_goal(&mut self, slot: Option<Slot>) {
        let previous = self.goal;
        self.goal = slot;
        self.reseat(previous, slot, SlotRef::Goal);
    }

    /// One turn's blocks moved: every cached position at or past `at` in
    /// `turn` shifts by `by` (`1` for an insert, `-1` for a removal).
    ///
    /// Touches that turn's slots only — the reason [`Slots::by_turn`] exists.
    pub(crate) fn shift(&mut self, turn: TurnKey, at: usize, by: isize) {
        let Some(refs) = self.by_turn.get(&turn) else { return };
        // Cloned because the buckets are read while the maps are written; a
        // bucket holds only the slots of one turn, so this is small.
        let refs = refs.clone();
        for slot_ref in refs {
            let slot = match &slot_ref {
                SlotRef::Item(id) => self.items.get_mut(id),
                SlotRef::Approval(id) => self.approvals.get_mut(id),
                SlotRef::Input(id, index) => {
                    self.inputs.get_mut(id).and_then(|slots| slots.get_mut(*index))
                }
                SlotRef::Todo => self.todo.as_mut(),
                SlotRef::Goal => self.goal.as_mut(),
            };
            let Some(slot) = slot else { continue };
            if slot.turn != turn {
                continue;
            }
            let moves = if by >= 0 { slot.block >= at } else { slot.block > at };
            if moves {
                slot.block = slot.block.saturating_add_signed(by);
            }
        }
    }

    /// A turn is gone: forget every position it hosted.
    ///
    /// A `userInput` request loses **all** its question cards when any one of
    /// them was in the removed turn — half a question set is not a question
    /// set, and the fold would have no way to draw the rest.
    pub(crate) fn drop_turn(&mut self, turn: TurnKey) {
        let Some(refs) = self.by_turn.remove(&turn) else { return };
        for slot_ref in refs {
            match slot_ref {
                SlotRef::Item(id) => {
                    self.items.remove(&id);
                }
                SlotRef::Approval(id) => {
                    self.approvals.remove(&id);
                }
                SlotRef::Input(id, _) => {
                    if let Some(slots) = self.inputs.remove(&id) {
                        for (index, slot) in slots.into_iter().enumerate() {
                            self.unlink(slot.turn, &SlotRef::Input(id.clone(), index));
                        }
                    }
                }
                SlotRef::Todo => self.todo = None,
                SlotRef::Goal => self.goal = None,
            }
        }
    }

    /// How many positions are cached, for the fold's own tests.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.items.len()
            + self.approvals.len()
            + self.inputs.values().map(Vec::len).sum::<usize>()
            + usize::from(self.todo.is_some())
            + usize::from(self.goal.is_some())
    }

    fn relink(
        &mut self,
        previous: Option<Slot>,
        slot: Slot,
        slot_ref: impl Fn() -> SlotRef,
    ) {
        match previous {
            Some(previous) if previous.turn == slot.turn => {}
            Some(previous) => {
                self.unlink(previous.turn, &slot_ref());
                self.link(slot.turn, slot_ref());
            }
            None => self.link(slot.turn, slot_ref()),
        }
    }

    fn reseat(&mut self, previous: Option<Slot>, slot: Option<Slot>, slot_ref: SlotRef) {
        if let Some(previous) = previous {
            if Some(previous.turn) != slot.map(|slot| slot.turn) {
                self.unlink(previous.turn, &slot_ref);
            }
        }
        if let Some(slot) = slot {
            if previous.map(|previous| previous.turn) != Some(slot.turn) {
                self.link(slot.turn, slot_ref);
            }
        }
    }

    fn link(&mut self, turn: TurnKey, slot_ref: SlotRef) {
        let bucket = self.by_turn.entry(turn).or_default();
        if !bucket.contains(&slot_ref) {
            bucket.push(slot_ref);
        }
    }

    fn unlink(&mut self, turn: TurnKey, slot_ref: &SlotRef) {
        if let Some(bucket) = self.by_turn.get_mut(&turn) {
            bucket.retain(|held| held != slot_ref);
            if bucket.is_empty() {
                self.by_turn.remove(&turn);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slot(turn: u32, block: usize) -> Slot {
        Slot { turn: TurnKey(turn), block }
    }

    #[test]
    fn a_shift_moves_only_the_named_turns_slots() {
        let mut slots = Slots::default();
        slots.set_item("a", slot(1, 0));
        slots.set_item("b", slot(1, 2));
        slots.set_item("c", slot(2, 0));
        slots.shift(TurnKey(1), 1, 1);
        assert_eq!(slots.item("a"), Some(slot(1, 0)), "before the insert point");
        assert_eq!(slots.item("b"), Some(slot(1, 3)), "past the insert point");
        assert_eq!(slots.item("c"), Some(slot(2, 0)), "another turn is untouched");
    }

    #[test]
    fn a_removal_shift_skips_the_hole_itself() {
        let mut slots = Slots::default();
        slots.set_item("a", slot(1, 1));
        slots.set_item("b", slot(1, 2));
        slots.shift(TurnKey(1), 1, -1);
        assert_eq!(slots.item("a"), Some(slot(1, 1)), "the removed block's own index");
        assert_eq!(slots.item("b"), Some(slot(1, 1)));
    }

    #[test]
    fn moving_a_slot_between_turns_moves_its_index_entry() {
        let mut slots = Slots::default();
        slots.set_item("a", slot(1, 0));
        slots.set_item("a", slot(2, 0));
        slots.shift(TurnKey(1), 0, 1);
        assert_eq!(slots.item("a"), Some(slot(2, 0)), "the old turn no longer names it");
        slots.shift(TurnKey(2), 0, 1);
        assert_eq!(slots.item("a"), Some(slot(2, 1)));
    }

    #[test]
    fn dropping_a_turn_forgets_everything_it_hosted() {
        let mut slots = Slots::default();
        slots.set_item("a", slot(1, 0));
        slots.set_approval("x", slot(1, 1));
        slots.set_inputs("q", vec![slot(1, 2), slot(1, 3)]);
        slots.set_todo(Some(slot(1, 4)));
        slots.set_goal(Some(slot(2, 0)));
        slots.drop_turn(TurnKey(1));
        assert_eq!(slots.item("a"), None);
        assert_eq!(slots.approval("x"), None);
        assert_eq!(slots.inputs("q"), None);
        assert_eq!(slots.todo(), None);
        assert_eq!(slots.goal(), Some(slot(2, 0)), "another turn's card survives");
        assert_eq!(slots.len(), 1);
    }
}
