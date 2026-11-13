//! The table of a sheet's cells as Tika's `TikaHSSFListener` writes it, which the spreadsheet
//! parsers share: a `<div class="page">` headed by the sheet's name, a table whose first row and
//! cell are open from the start, a row ended and another started where the cells move to a later
//! row, and an empty cell for each column skipped. A sheet whose cells start past the first row
//! therefore begins with an empty row, and a sheet of no cells is a table of one empty cell.

use crate::formats::xhtml::{Stop, Xhtml};

/// Where the elements of a sheet's table are written.
pub trait Markup {
    fn start(&mut self, name: &'static str) -> Result<(), Stop>;
    fn start_class(&mut self, name: &'static str, class: &'static str) -> Result<(), Stop>;
    fn end(&mut self, name: &'static str) -> Result<(), Stop>;
    /// `XHTMLContentHandler.element`: the element with its text, unless that is empty.
    fn element(&mut self, name: &'static str, text: &str) -> Result<(), Stop>;
}

impl Markup for Xhtml<'_> {
    fn start(&mut self, name: &'static str) -> Result<(), Stop> {
        self.start_element(name)
    }

    fn start_class(&mut self, name: &'static str, class: &'static str) -> Result<(), Stop> {
        self.start_element_with(name, &[("class", class)])
    }

    fn end(&mut self, name: &'static str) -> Result<(), Stop> {
        self.end_element(name)
    }

    fn element(&mut self, name: &'static str, text: &str) -> Result<(), Stop> {
        Xhtml::element(self, name, text)
    }
}

/// A sheet's table being written: the row and column of the open cell.
pub struct SheetTable {
    row: i32,
    column: i32,
    /// Whether a row skipped between two cells is written as an empty row, as Tika's
    /// `includeMissingRows` has it, rather than left out.
    missing_rows: bool,
}

impl SheetTable {
    /// Starts the page of a sheet, headed with its name, its table, and the first row and cell.
    pub fn start(
        name: Option<&str>,
        missing_rows: bool,
        out: &mut (impl Markup + ?Sized),
    ) -> Result<SheetTable, Stop> {
        out.start_class("div", "page")?;
        if let Some(name) = name {
            out.element("h1", name)?;
        }
        for name in ["table", "tbody", "tr", "td"] {
            out.start(name)?;
        }
        Ok(SheetTable {
            row: 0,
            column: 0,
            missing_rows,
        })
    }

    /// Opens the cell at `place`, which comes after the cells entered before it, for its content
    /// to be written next.
    pub fn enter(
        &mut self,
        (row, column): (i32, i32),
        out: &mut (impl Markup + ?Sized),
    ) -> Result<(), Stop> {
        if self.row != row {
            loop {
                out.end("td")?;
                out.end("tr")?;
                out.start("tr")?;
                out.start("td")?;
                self.row += 1;
                if !(self.missing_rows && self.row < row) {
                    break;
                }
            }
            self.row = row;
            self.column = 0;
        }
        while self.column < column {
            out.end("td")?;
            out.start("td")?;
            self.column += 1;
        }
        Ok(())
    }

    /// Ends the open cell and row and the table, leaving the page open for what follows the
    /// table in it.
    pub fn end(self, out: &mut (impl Markup + ?Sized)) -> Result<(), Stop> {
        for name in ["td", "tr", "tbody", "table"] {
            out.end(name)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The elements written, as tags, a heading with its text.
    #[derive(Default)]
    struct Tags(String);

    impl Markup for Tags {
        fn start(&mut self, name: &'static str) -> Result<(), Stop> {
            self.0.push_str(&format!("<{name}>"));
            Ok(())
        }

        fn start_class(&mut self, name: &'static str, class: &'static str) -> Result<(), Stop> {
            self.0.push_str(&format!("<{name} class={class}>"));
            Ok(())
        }

        fn end(&mut self, name: &'static str) -> Result<(), Stop> {
            self.0.push_str(&format!("</{name}>"));
            Ok(())
        }

        fn element(&mut self, name: &'static str, text: &str) -> Result<(), Stop> {
            self.0.push_str(&format!("<{name}>{text}</{name}>"));
            Ok(())
        }
    }

    fn written(missing_rows: bool, places: &[(i32, i32)]) -> String {
        let mut tags = Tags::default();
        let mut table = SheetTable::start(Some("One"), missing_rows, &mut tags).unwrap();
        for &place in places {
            table.enter(place, &mut tags).unwrap();
            tags.0.push('x');
        }
        table.end(&mut tags).unwrap();
        tags.0
    }

    #[test]
    fn opens_each_cell_after_the_empty_cells_and_rows_before_it() {
        assert_eq!(
            written(false, &[(0, 1), (0, 2), (3, 0)]),
            "<div class=page><h1>One</h1><table><tbody><tr><td></td><td>x</td><td>x</td></tr>\
             <tr><td>x</td></tr></tbody></table>"
        );
        assert_eq!(
            written(true, &[(1, 0), (3, 0)]),
            "<div class=page><h1>One</h1><table><tbody><tr><td></td></tr><tr><td>x</td></tr>\
             <tr><td></td></tr><tr><td>x</td></tr></tbody></table>"
        );
        assert_eq!(
            written(false, &[]),
            "<div class=page><h1>One</h1><table><tbody><tr><td></td></tr></tbody></table>"
        );
    }
}
