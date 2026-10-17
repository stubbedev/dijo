//! The scanning loop of a JFlex 1.4.1 scanner (`yylex`), run on the tables of jhighlight's
//! highlighters: from the lexical state's start, the transitions of each UTF-16 unit's
//! character class until none leads on, the longest match being the last accepting state met.

use std::sync::OnceLock;

/// A scanner's tables, each run-length encoded as (count, value) pairs.
pub(super) struct Tables {
    pub(super) character_classes: &'static [(u32, i32)],
    pub(super) actions: &'static [(u32, i32)],
    pub(super) rows: &'static [(u32, i32)],
    pub(super) transitions: &'static [(u32, i32)],
    pub(super) attributes: &'static [(u32, i32)],
}

/// A scanner's tables, unpacked.
pub(super) struct Scanner {
    character_classes: Vec<i32>,
    actions: Vec<i32>,
    rows: Vec<i32>,
    transitions: Vec<i32>,
    attributes: Vec<i32>,
}

/// `ZZ_ATTRIBUTE`'s flag of an accepting state.
const ACCEPTING: i32 = 1;
/// `ZZ_ATTRIBUTE`'s flag of a state no transition leaves.
const FINAL: i32 = 8;

fn unpack(runs: &[(u32, i32)]) -> Vec<i32> {
    let mut values = Vec::with_capacity(runs.iter().map(|&(count, _)| count as usize).sum());
    for &(count, value) in runs {
        values.extend(std::iter::repeat_n(value, count as usize));
    }
    values
}

impl Scanner {
    /// The scanner of `tables`, unpacked once into `cell`.
    pub(super) fn of(cell: &'static OnceLock<Scanner>, tables: &Tables) -> &'static Scanner {
        cell.get_or_init(|| Scanner {
            character_classes: unpack(tables.character_classes),
            actions: unpack(tables.actions),
            rows: unpack(tables.rows),
            transitions: unpack(tables.transitions),
            attributes: unpack(tables.attributes),
        })
    }

    /// The next token of `units` from `start` in the lexical `state`: the action of the longest
    /// match, and its length. `None` where no rule matches, which the scanners' fallback rule
    /// rules out but on an empty input.
    pub(super) fn next(&self, units: &[u16], start: usize, state: usize) -> Option<(i32, usize)> {
        let mut dfa = state;
        let mut action = None;
        let mut marked = start;
        let mut current = start;
        while let Some(&unit) = units.get(current) {
            current += 1;
            let class = self.character_classes[usize::from(unit)];
            let next = self.transitions[(self.rows[dfa] + class) as usize];
            if next == -1 {
                break;
            }
            dfa = next as usize;
            let attributes = self.attributes[dfa];
            if attributes & ACCEPTING == ACCEPTING {
                action = Some(self.actions[dfa]);
                marked = current;
                if attributes & FINAL == FINAL {
                    break;
                }
            }
        }
        action.map(|action| (action, marked - start))
    }
}
