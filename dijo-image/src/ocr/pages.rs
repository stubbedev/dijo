//! Our Java `PageReadings`: the pages of a document read by OCR side by side, on as many threads
//! as the process may use processors, while the document is written and passes each page's
//! output on in order. Each page is first read in the script and model the latest page before it
//! that was read in one was, or in those assumed before the first page, so that a document in one
//! language is read with its model from its first page of text on, a cover or a notice in another
//! language before it notwithstanding, and every page reads the same whichever finishes first. A
//! page waits for the pages before it while none of them has been read in anything, rather than
//! load the assumed script's model for each page read side by side.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex, MutexGuard};

use crate::formats::embedded::OcrLanguage;
use crate::memory;

/// What the pages of a document are first read in, such as a script and model: what the latest
/// page before each that was read in one was read in, or what was assumed before the first page.
pub struct Learning<L = OcrLanguage> {
    assumed: Option<L>,
    state: Mutex<Learned<L>>,
    recorded: Condvar,
}

struct Learned<L> {
    /// What each page was read in, by its index, once it is read: none for a page read in
    /// nothing, such as noise or a page that could not be read.
    pages: Vec<Option<Option<L>>>,
    /// How many pages from the first are read.
    read: usize,
}

impl<L: Clone + PartialEq> Learning<L> {
    pub fn new(assumed: Option<L>) -> Self {
        Self {
            assumed,
            state: Mutex::new(Learned {
                pages: Vec::new(),
                read: 0,
            }),
            recorded: Condvar::new(),
        }
    }

    /// What the extraction assumes after the pages: what the latest of the pages read from the
    /// first on was read in, or none when none was read in anything.
    pub fn language(&self) -> Option<L> {
        let state = self.state();
        Self::latest(&state.pages[..state.read])
    }

    fn state(&self) -> MutexGuard<'_, Learned<L>> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// What the latest of `pages` that is read and was read in something was read in.
    fn latest(pages: &[Option<Option<L>>]) -> Option<L> {
        pages
            .iter()
            .rev()
            .find_map(|page| page.as_ref().and_then(Option::as_ref))
            .cloned()
    }

    /// What page `index` is read in as far as the pages before it read so far tell.
    fn before(&self, state: &Learned<L>, index: usize) -> Option<L> {
        Self::latest(&state.pages[..index.min(state.pages.len())]).or_else(|| self.assumed.clone())
    }

    /// What page `index` is first read in, and whether that is settled: what the pages before
    /// it settle once every one is read, else what the latest of those read so far was read in.
    /// While none of those read so far was read in anything, it waits rather than guess what was
    /// assumed: a guess in the script first assumed loads that script's model, the largest by
    /// far for Latin, once for each page read side by side, only to read most of them again in
    /// the language the first page settles.
    fn guess(&self, index: usize) -> (Option<L>, bool) {
        let mut state = self.state();
        loop {
            if state.read >= index {
                return (self.before(&state, index), true);
            }
            if let Some(latest) = Self::latest(&state.pages[..index.min(state.pages.len())]) {
                return (Some(latest), false);
            }
            state = self.recorded.wait(state).unwrap_or_else(|e| e.into_inner());
        }
    }

    /// What page `index` is read in, waiting until every page before it is read.
    fn settled(&self, index: usize) -> Option<L> {
        let mut state = self.state();
        while state.read < index {
            state = self.recorded.wait(state).unwrap_or_else(|e| e.into_inner());
        }
        self.before(&state, index)
    }

    /// Records what page `index` was read in. Only the first record of a page counts, so that a
    /// page whose reading ended without one is recorded as read in nothing.
    fn record(&self, index: usize, language: Option<L>) {
        let mut state = self.state();
        if state.pages.len() <= index {
            state.pages.resize(index + 1, None);
        }
        if state.pages[index].is_some() {
            return;
        }
        state.pages[index] = Some(language);
        let Learned { pages, read } = &mut *state;
        while pages.get(*read).is_some_and(Option::is_some) {
            *read += 1;
        }
        self.recorded.notify_all();
    }

    /// Reads page `index` in what the pages before it settle. A page read before every page
    /// before it is read is read in what the latest of those read so far was read in, once one
    /// was read in something, and read again if the rest settle otherwise; a failure of that
    /// guess counts only when it stands.
    fn read<T, E>(
        &self,
        index: usize,
        mut read: impl FnMut(Option<&L>) -> Result<(T, Option<L>), E>,
    ) -> Result<T, E> {
        let _record = RecordOnExit {
            learning: self,
            index,
        };
        let (guess, settled) = self.guess(index);
        let mut output = read(guess.as_ref());
        if !settled {
            let settled = self.settled(index);
            if settled != guess {
                output = read(settled.as_ref());
            }
        }
        let (output, language) = match output {
            Ok((output, language)) => (Ok(output), language),
            Err(failure) => (Err(failure), None),
        };
        self.record(index, language);
        output
    }
}

/// Records a page as read in nothing when its reading ends without recording it.
struct RecordOnExit<'a, L: Clone + PartialEq> {
    learning: &'a Learning<L>,
    index: usize,
}

impl<L: Clone + PartialEq> Drop for RecordOnExit<'_, L> {
    fn drop(&mut self) {
        self.learning.record(self.index, None);
    }
}

/// The outputs of the pages, kept until they are passed on.
struct Shared<T, E> {
    outputs: Mutex<Vec<Option<Result<T, E>>>>,
    ready: Condvar,
    /// Whether the pages still to be read are given up, as the document stopped being written.
    abandoned: AtomicBool,
    /// Whether a reading failed with a panic, which the pages waiting for it then share.
    panicked: AtomicBool,
}

/// Marks a reading thread that panics, so that no one waits for its page forever.
struct Reading<'a, T, E>(&'a Shared<T, E>);

impl<T, E> Drop for Reading<'_, T, E> {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.0.panicked.store(true, Ordering::SeqCst);
            let _outputs = self.0.outputs.lock().unwrap_or_else(|e| e.into_inner());
            self.0.ready.notify_all();
        }
    }
}

/// The outputs of pages read side by side, passed on in page order.
pub struct Outputs<'a, T, E> {
    shared: &'a Shared<T, E>,
    next: usize,
}

impl<T, E> Iterator for Outputs<'_, T, E> {
    type Item = Result<T, E>;

    /// The output of the next page, once it is read; none after the last.
    fn next(&mut self) -> Option<Result<T, E>> {
        let mut outputs = self
            .shared
            .outputs
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if self.next >= outputs.len() {
            return None;
        }
        loop {
            if let Some(output) = outputs[self.next].take() {
                self.next += 1;
                return Some(output);
            }
            assert!(
                !self.shared.panicked.load(Ordering::SeqCst),
                "reading a page by OCR panicked"
            );
            outputs = self
                .shared
                .ready
                .wait(outputs)
                .unwrap_or_else(|e| e.into_inner());
        }
    }
}

/// Reads `count` pages side by side on the processors the process may use, while `consume`
/// takes their outputs in page order. Each thread reads its pages with a `state` of its own,
/// made when it reads its first: `read` reads a page in a script and model, or the configured
/// language, and tells what it read and the script and model it was read in. Each page is read in
/// what `learning` settles for it. When `consume` returns, the pages not yet read are given up,
/// and those being read are waited for.
///
/// Under a memory budget the pages are read one at a time, on a thread whose allocations are
/// charged to the budget of the thread that called: a page takes the memory of its raster, the
/// images prepared from it and tesseract's reading of them, which pages read side by side would
/// multiply, by whichever happen to be read at once.
pub fn read_pages<S, T: Send, E: Send, R, L: Clone + PartialEq + Send + Sync>(
    count: usize,
    learning: &Learning<L>,
    state: impl Fn() -> S + Sync,
    read: impl Fn(&mut S, usize, Option<&L>) -> Result<(T, Option<L>), E> + Sync,
    consume: impl FnOnce(&mut Outputs<'_, T, E>) -> R,
) -> R {
    let shared = Shared {
        outputs: Mutex::new((0..count).map(|_| None).collect()),
        ready: Condvar::new(),
        abandoned: AtomicBool::new(false),
        panicked: AtomicBool::new(false),
    };
    let next = AtomicUsize::new(0);
    let budget = memory::share();
    let threads = match budget {
        Some(_) => 1,
        None => crate::process::processors().min(count).max(1),
    };
    std::thread::scope(|scope| {
        for _ in 0..threads {
            let (shared, next, state, read, budget) = (&shared, &next, &state, &read, &budget);
            scope.spawn(move || match budget {
                Some(budget) => {
                    budget.within(|| read_in_turn(shared, next, count, learning, state, read))
                }
                None => read_in_turn(shared, next, count, learning, state, read),
            });
        }
        let mut outputs = Outputs {
            shared: &shared,
            next: 0,
        };
        let consumed = consume(&mut outputs);
        shared.abandoned.store(true, Ordering::SeqCst);
        consumed
    })
}

/// Reads the next page not yet taken, on this thread, until none is left or they are given up.
fn read_in_turn<S, T, E, L: Clone + PartialEq>(
    shared: &Shared<T, E>,
    next: &AtomicUsize,
    count: usize,
    learning: &Learning<L>,
    state: &impl Fn() -> S,
    read: &impl Fn(&mut S, usize, Option<&L>) -> Result<(T, Option<L>), E>,
) {
    let _reading = Reading(shared);
    let mut own = None;
    loop {
        if shared.abandoned.load(Ordering::SeqCst) {
            return;
        }
        let index = next.fetch_add(1, Ordering::SeqCst);
        if index >= count {
            return;
        }
        let own = own.get_or_insert_with(state);
        let output = learning.read(index, |language| read(own, index, language));
        let mut outputs = shared.outputs.lock().unwrap_or_else(|e| e.into_inner());
        outputs[index] = Some(output);
        shared.ready.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_all<T, E>(outputs: &mut Outputs<'_, T, E>) -> Vec<Result<T, E>> {
        std::iter::from_fn(|| outputs.next()).collect()
    }

    /// Reads pages each written in the script `written` gives it, the slower the earlier when
    /// `staggered`, so later pages finish first: what each page was read in, in page order, and
    /// what the extraction assumes after them.
    fn read_written(
        count: usize,
        assumed: Option<&str>,
        written: impl Fn(usize) -> Option<&'static str> + Sync,
        staggered: bool,
    ) -> (Vec<String>, Option<String>) {
        let learning = Learning::new(assumed.map(str::to_owned));
        let read = read_pages(
            count,
            &learning,
            || (),
            |(), page, script| {
                if staggered {
                    let millis = (count - page) as u64 * 3 + (page as u64 * 7) % 5;
                    std::thread::sleep(std::time::Duration::from_millis(millis));
                }
                Ok::<_, ()>((
                    format!("{page}:{}", script.map_or("-", String::as_str)),
                    written(page).map(str::to_owned),
                ))
            },
            read_all,
        );
        (
            read.into_iter().map(Result::unwrap).collect(),
            learning.language(),
        )
    }

    #[test]
    fn reads_later_pages_in_the_script_the_first_was_read_in() {
        let (read, language) = read_written(6, Some("Latin"), |_| Some("Cyrillic"), true);
        assert_eq!(
            read,
            [
                "0:Latin",
                "1:Cyrillic",
                "2:Cyrillic",
                "3:Cyrillic",
                "4:Cyrillic",
                "5:Cyrillic"
            ]
        );
        assert_eq!(language.as_deref(), Some("Cyrillic"));
    }

    #[test]
    fn reads_each_page_in_what_the_latest_page_before_it_was_read_in() {
        let written = |page| match page {
            0 => None,
            1 => Some("eng"),
            _ => Some("spa"),
        };
        let (read, language) = read_written(6, Some("Latin"), written, true);
        assert_eq!(
            read,
            ["0:Latin", "1:Latin", "2:eng", "3:spa", "4:spa", "5:spa"]
        );
        assert_eq!(language.as_deref(), Some("spa"));
    }

    #[test]
    fn reads_alike_whichever_page_finishes_first() {
        let written = |page| match page {
            7 | 8 => None,
            0..=9 => Some("eng"),
            10..=29 => Some("spa"),
            _ => Some("fra"),
        };
        let expected: Vec<String> = (0..40)
            .map(|page| {
                let before = (0..page).rev().find_map(written).unwrap_or("Latin");
                format!("{page}:{before}")
            })
            .collect();
        for staggered in [false, true] {
            let (read, language) = read_written(40, Some("Latin"), written, staggered);
            assert_eq!(read, expected);
            assert_eq!(language.as_deref(), Some("fra"));
        }
    }

    #[test]
    fn reads_no_page_in_what_was_assumed_while_the_first_settles_otherwise() {
        let calls = Mutex::new(Vec::new());
        let learning = Learning::new(Some("Latin".to_owned()));
        read_pages(
            6,
            &learning,
            || (),
            |(), page, script| {
                if page == 0 {
                    std::thread::sleep(std::time::Duration::from_millis(30));
                }
                calls
                    .lock()
                    .unwrap()
                    .push((page, script.cloned().unwrap_or_default()));
                Ok::<_, ()>((page, Some("Cyrillic".to_owned())))
            },
            read_all,
        );
        let mut calls = calls.into_inner().unwrap();
        calls.sort();
        assert_eq!(
            calls,
            [
                (0, "Latin".to_owned()),
                (1, "Cyrillic".to_owned()),
                (2, "Cyrillic".to_owned()),
                (3, "Cyrillic".to_owned()),
                (4, "Cyrillic".to_owned()),
                (5, "Cyrillic".to_owned()),
            ]
        );
    }

    #[test]
    fn keeps_the_assumed_script_when_the_first_page_fails() {
        let learning = Learning::<String>::new(None);
        let read = read_pages(
            3,
            &learning,
            || (),
            |(), page, _| {
                if page == 0 {
                    Err("unreadable")
                } else {
                    Ok((page, None))
                }
            },
            read_all,
        );
        assert_eq!(read, [Err("unreadable"), Ok(1), Ok(2)]);
        assert_eq!(learning.language(), None);
    }

    #[test]
    fn gives_up_the_pages_left_when_the_output_stops() {
        let learning = Learning::<String>::new(None);
        let started = AtomicUsize::new(0);
        let pages = 100 * crate::process::processors();
        let first = read_pages(
            pages,
            &learning,
            || (),
            |(), page, _| {
                started.fetch_add(1, Ordering::SeqCst);
                std::thread::sleep(std::time::Duration::from_millis(10));
                Ok::<_, ()>((page, None))
            },
            |outputs| outputs.next(),
        );
        assert_eq!(first, Some(Ok(0)));
        assert!(started.load(Ordering::SeqCst) < pages / 2);
    }
}
