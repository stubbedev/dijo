//! Tables tesseract finds on a page, written as born-digital tables are: a row a line, its cells
//! separated by tabs. Tesseract's layout analysis marks a block of words aligned in columns as a
//! table, but reads it as lines of words separated by spaces. The columns are the runs of the
//! block's width that its words cover, split where a gap no word of any row crosses is wider than
//! a word space: a gutter, which runs down every row of the table.

use std::ffi::c_int;

/// The fewest rows of a table that hold words in more than one of its columns.
const MIN_ROWS: usize = 3;
/// The narrowest gutter between columns, in eighths of the median height of the table's words:
/// a word space is under half the height of a word, while the gutters of tables are at least
/// a word's height wide.
const MIN_GUTTER_EIGHTHS: c_int = 6;

/// A word of a row of a table, by its edges on the page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Word {
    pub(crate) left: c_int,
    pub(crate) right: c_int,
    pub(crate) height: c_int,
}

/// The text tesseract read of a table block, `text`, with the words of each of its lines in
/// `rows`, written as a table: the words of a cell separated by spaces, the cells of a row by a
/// tab each, an empty cell by a tab of its own. None when its words do not fall into [`columns`],
/// or its lines do not hold the words found in them.
pub(crate) fn tabulated(text: &str, rows: &[Vec<Word>]) -> Option<String> {
    let columns = columns(rows)?;
    let mut rows = rows.iter();
    let mut table = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        let content = line.trim_end_matches('\n');
        if content.trim().is_empty() {
            table.push_str(line);
            continue;
        }
        let row = rows.next()?;
        let mut tokens = content.split_whitespace();
        let mut previous: Option<usize> = None;
        let rightwards = row.first()?.left <= row.last()?.left;
        for word in row {
            let token = tokens.next()?;
            let column = column_of(&columns, word)?;
            let column = match rightwards {
                true => column,
                false => columns.len() - 1 - column,
            };
            match previous {
                Some(before) if before == column => table.push(' '),
                Some(before) => push_tabs(&mut table, column.abs_diff(before)),
                None => push_tabs(&mut table, column),
            }
            table.push_str(token);
            previous = Some(column);
        }
        if tokens.next().is_some() {
            return None;
        }
        table.push_str(&line[content.len()..]);
    }
    rows.next().is_none().then_some(table)
}

fn push_tabs(table: &mut String, count: usize) {
    table.extend(std::iter::repeat_n('\t', count));
}

/// The columns of a table, left to right: the runs of its width its words cover, joined across
/// gaps narrower than a gutter. None unless there are at least two, and at least [`MIN_ROWS`]
/// rows hold words in more than one of them.
fn columns(rows: &[Vec<Word>]) -> Option<Vec<Column>> {
    let mut words: Vec<&Word> = rows.iter().flatten().collect();
    let mut heights: Vec<c_int> = words.iter().map(|word| word.height).collect();
    heights.sort_unstable();
    let gutter = heights.get(heights.len() / 2)? * MIN_GUTTER_EIGHTHS / 8;
    words.sort_unstable_by_key(|word| word.left);
    let mut columns: Vec<Column> = Vec::new();
    for word in words {
        match columns.last_mut() {
            Some(column) if word.left - column.right < gutter => {
                column.right = column.right.max(word.right);
            }
            _ => columns.push(Column {
                left: word.left,
                right: word.right,
            }),
        }
    }
    let spanning = rows
        .iter()
        .filter(|row| {
            row.split_first().is_some_and(|(first, others)| {
                others
                    .iter()
                    .any(|word| column_of(&columns, word) != column_of(&columns, first))
            })
        })
        .count();
    (columns.len() >= 2 && spanning >= MIN_ROWS).then_some(columns)
}

/// The index of the column of `columns` a word lies in.
fn column_of(columns: &[Column], word: &Word) -> Option<usize> {
    columns.iter().position(|column| column.contains(word))
}

/// A column of a table, by its edges on the page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Column {
    left: c_int,
    right: c_int,
}

impl Column {
    fn contains(&self, word: &Word) -> bool {
        self.left <= word.left && word.right <= self.right
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(left: c_int, right: c_int) -> Word {
        Word {
            left,
            right,
            height: 40,
        }
    }

    fn lighthouse_rows() -> Vec<Vec<Word>> {
        let header = vec![
            word(300, 410),
            word(500, 620),
            word(910, 1010),
            word(1300, 1420),
            word(1435, 1480),
        ];
        let row = vec![
            word(305, 420),
            word(510, 600),
            word(910, 1150),
            word(1300, 1420),
        ];
        vec![header, row.clone(), row.clone(), row]
    }

    #[test]
    fn writes_the_cells_of_a_row_separated_by_tabs_and_their_words_by_spaces() {
        let text = "Year Ships Wind Lamp lit\n\
                    1921 412 south-west 17:05\n\
                    1922 389 west 17:12\n\
                    1923 455 north-west 16:58\n\n";
        assert_eq!(
            tabulated(text, &lighthouse_rows()).as_deref(),
            Some(
                "Year\tShips\tWind\tLamp lit\n\
                 1921\t412\tsouth-west\t17:05\n\
                 1922\t389\twest\t17:12\n\
                 1923\t455\tnorth-west\t16:58\n\n"
            )
        );
    }

    #[test]
    fn writes_a_tab_for_each_empty_cell() {
        let rows = vec![
            vec![word(0, 100), word(300, 400), word(600, 700)],
            vec![word(0, 100), word(600, 700)],
            vec![word(300, 400), word(600, 700)],
        ];
        assert_eq!(
            tabulated("a b c\nd f\ng h\n", &rows).as_deref(),
            Some("a\tb\tc\nd\t\tf\n\tg\th\n")
        );
    }

    #[test]
    fn writes_the_cells_of_a_right_to_left_row_in_its_order() {
        let rows = vec![
            vec![word(600, 700), word(300, 400), word(0, 100)],
            vec![word(600, 700), word(300, 400), word(0, 100)],
            vec![word(600, 700), word(0, 100)],
        ];
        assert_eq!(
            tabulated("א ב ג\nד ה ו\nז ח\n", &rows).as_deref(),
            Some("א\tב\tג\nד\tה\tו\nז\t\tח\n")
        );
    }

    #[test]
    fn leaves_lines_whose_word_gaps_do_not_line_up_as_they_are() {
        let rows = vec![
            vec![word(0, 100), word(115, 300), word(315, 500)],
            vec![word(0, 200), word(215, 340), word(355, 500)],
            vec![word(0, 60), word(75, 260), word(275, 500)],
        ];
        assert_eq!(tabulated("a b c\nd e f\ng h i\n", &rows), None);
    }

    #[test]
    fn leaves_a_block_of_fewer_rows_in_columns_as_it_is() {
        let rows = &lighthouse_rows()[..2];
        assert_eq!(tabulated("a b c d e\nf g h i\n", rows), None);
    }

    #[test]
    fn leaves_lines_that_do_not_hold_the_words_found_in_them_as_they_are() {
        let text = "Year Ships Wind Lamp lit\n1921 412 south-west\n1922 389 west 17:12\n";
        assert_eq!(tabulated(text, &lighthouse_rows()), None);
    }
}
