//! The table-driven execution loop Ragel 6 generates for Java (`-T0`), over the tables of
//! [`super::tables`]: each key moves the machine along a transition and runs that transition's
//! actions; at the end of the input, the end-of-file actions of the state it stopped in run.

/// A machine's tables, as Ragel writes them; `indicies` is empty for a machine that indexes
/// its targets directly.
pub(super) struct Tables {
    pub(super) actions: &'static [u16],
    pub(super) key_offsets: &'static [u16],
    pub(super) trans_keys: &'static [u16],
    pub(super) single_lengths: &'static [u16],
    pub(super) range_lengths: &'static [u16],
    pub(super) index_offsets: &'static [u16],
    pub(super) indicies: &'static [u16],
    pub(super) trans_targs: &'static [u16],
    pub(super) trans_actions: &'static [u16],
    pub(super) eof_actions: &'static [u16],
}

/// The state every machine fails into.
pub(super) const ERROR: usize = 0;

/// What an action asks of the loop: to go on, or to stop after the current key (`fbreak`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Flow {
    Continue,
    Break,
}

impl Tables {
    /// Runs the machine in state `cs` over `keys` from `start`: `on_action` runs each action with
    /// the position of the key it fires on. Stops at the end of the keys, at the key that takes
    /// it into the error state, or just past the key of an action that breaks; then, when the
    /// keys end the input and the machine reached their end, runs the end-of-file actions of its
    /// state. Returns the position it stopped at.
    pub(super) fn exec<E>(
        &self,
        cs: &mut usize,
        keys: &[i32],
        start: usize,
        end_of_input: bool,
        on_action: &mut dyn FnMut(u16, usize) -> Result<Flow, E>,
    ) -> Result<usize, E> {
        let mut p = start;
        if *cs == ERROR && p < keys.len() {
            return Ok(p);
        }
        while p < keys.len() {
            let transition = self.transition(*cs, keys[p]);
            *cs = usize::from(self.trans_targs[transition]);
            for &action in self.action_list(self.trans_actions[transition]) {
                if on_action(action, p)? == Flow::Break {
                    return Ok(p + 1);
                }
            }
            if *cs == ERROR {
                return Ok(p);
            }
            p += 1;
        }
        if end_of_input {
            for &action in self.action_list(self.eof_actions.get(*cs).copied().unwrap_or(0)) {
                if on_action(action, p)? == Flow::Break {
                    break;
                }
            }
        }
        Ok(p)
    }

    /// The actions an entry of `trans_actions` or `eof_actions` names.
    fn action_list(&self, at: u16) -> &[u16] {
        if at == 0 {
            return &[];
        }
        let at = usize::from(at);
        let count = usize::from(self.actions[at]);
        &self.actions[at + 1..at + 1 + count]
    }

    /// The transition state `cs` takes on `key`: a binary search of its single keys, then of its
    /// ranges, else its default.
    fn transition(&self, cs: usize, key: i32) -> usize {
        let mut keys = usize::from(self.key_offsets[cs]);
        let mut transition = usize::from(self.index_offsets[cs]);
        let single = usize::from(self.single_lengths[cs]);
        let found = 'found: {
            if single > 0 {
                let singles = &self.trans_keys[keys..keys + single];
                if let Ok(index) = singles.binary_search_by(|&probe| i32::from(probe).cmp(&key)) {
                    break 'found transition + index;
                }
                keys += single;
                transition += single;
            }
            let ranges = usize::from(self.range_lengths[cs]);
            let pairs = &self.trans_keys[keys..keys + 2 * ranges];
            let index = pairs
                .as_chunks::<2>()
                .0
                .iter()
                .position(|&[low, high]| i32::from(low) <= key && key <= i32::from(high));
            match index {
                Some(index) => transition + index,
                None => transition + ranges,
            }
        };
        match self.indicies {
            [] => found,
            indicies => usize::from(indicies[found]),
        }
    }
}
