//! Our Java `PageReadings`: the pages of a document read by OCR side by side, on as many threads
//! as the process may use processors, while the document is written and passes each page's
//! output on in order. The first page is read as any image is; the rest are first read in the
//! script and model it leaves the extraction assuming, so that every page reads as it would after
//! the first, whichever finishes first.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex};

use crate::formats::embedded::OcrLanguage;
use crate::memory;

/// What the pages of a document after its first are first read in, such as a script and model:
/// what was assumed before the first page, unless reading it teaches otherwise.
pub struct FirstPage<L = OcrLanguage> {
    assumed: Option<L>,
    /// What the first page learned, once it is settled: none when it learned nothing.
    learned: Mutex<Option<Option<L>>>,
    settled: Condvar,
}

impl<L: Clone + PartialEq> FirstPage<L> {
    pub fn new(assumed: Option<L>) -> Self {
        Self {
            assumed,
            learned: Mutex::new(None),
            settled: Condvar::new(),
        }
    }

    /// What was assumed before the first page.
    pub(crate) fn assumed(&self) -> Option<&L> {
        self.assumed.as_ref()
    }

    /// Settles what the later pages are read in: what the first page learned, or, when it is
    /// none, what was assumed. Only the first call counts, so that a page that could not be read
    /// settles it as assumed.
    pub(crate) fn settle(&self, learned: Option<L>) {
        let mut settled = self.learned.lock().unwrap_or_else(|e| e.into_inner());
        if settled.is_none() {
            *settled = Some(learned);
            self.settled.notify_all();
        }
    }

    /// What the first page learned, once it is settled.
    pub fn learned(&self) -> Option<L> {
        self.learned
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .flatten()
    }

    /// What the later pages are read in, waiting until it is settled.
    fn settled(&self) -> Option<L> {
        let mut learned = self.learned.lock().unwrap_or_else(|e| e.into_inner());
        while learned.is_none() {
            learned = self
                .settled
                .wait(learned)
                .unwrap_or_else(|e| e.into_inner());
        }
        learned.clone().flatten().or_else(|| self.assumed.clone())
    }

    fn is_settled(&self) -> bool {
        self.learned
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
    }

    /// Reads a later page in what the first page settles. A page read before it is settled is
    /// read in what was assumed, and read again if the first page teaches otherwise; a failure
    /// of that guess counts only when it stands.
    fn later<T, E>(&self, mut read: impl FnMut(Option<&L>) -> Result<T, E>) -> Result<T, E> {
        if self.is_settled() {
            return read(self.settled().as_ref());
        }
        let guess = read(self.assumed());
        let settled = self.settled();
        if settled != self.assumed {
            return read(settled.as_ref());
        }
        guess
    }
}

/// Settles the first page as assumed when its reading ends without settling it.
struct SettleOnExit<'a, L: Clone + PartialEq>(&'a FirstPage<L>);

impl<L: Clone + PartialEq> Drop for SettleOnExit<'_, L> {
    fn drop(&mut self) {
        self.0.settle(None);
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
/// language, and tells what it read and what it learned. The first page is read in what
/// `first_page` assumes and settles what the rest are read in. When `consume` returns, the pages
/// not yet read are given up, and those being read are waited for.
///
/// Under a memory budget the pages are read one at a time, on a thread whose allocations are
/// charged to the budget of the thread that called: a page takes the memory of its raster, the
/// images prepared from it and tesseract's reading of them, which pages read side by side would
/// multiply, by whichever happen to be read at once.
pub fn read_pages<S, T: Send, E: Send, R, L: Clone + PartialEq + Send + Sync>(
    count: usize,
    first_page: &FirstPage<L>,
    state: impl Fn() -> S + Sync,
    read: impl Fn(&mut S, usize, Option<&L>) -> Result<(T, Option<L>), E> + Sync,
    consume: impl FnOnce(&mut Outputs<'_, T, E>) -> R,
) -> R {
    if count == 0 {
        first_page.settle(None);
    }
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
                    budget.within(|| read_in_turn(shared, next, count, first_page, state, read))
                }
                None => read_in_turn(shared, next, count, first_page, state, read),
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
    first_page: &FirstPage<L>,
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
        let output = if index == 0 {
            let _settle = SettleOnExit(first_page);
            read(own, index, first_page.assumed()).map(|(output, learned)| {
                first_page.settle(learned);
                output
            })
        } else {
            first_page.later(|assumed| read(own, index, assumed).map(|(output, _)| output))
        };
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

    #[test]
    fn reads_later_pages_in_the_script_the_first_learns() {
        let first_page = FirstPage::new(Some("Latin".to_owned()));
        let read = read_pages(
            6,
            &first_page,
            || (),
            |(), page, script| {
                if page == 0 {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                    return Ok::<_, ()>((
                        format!("{page}:{}", script.map_or("-", String::as_str)),
                        Some("Cyrillic".to_owned()),
                    ));
                }
                Ok((
                    format!("{page}:{}", script.map_or("-", String::as_str)),
                    None,
                ))
            },
            read_all,
        );
        let read: Vec<String> = read.into_iter().map(Result::unwrap).collect();
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
        assert_eq!(first_page.learned().as_deref(), Some("Cyrillic"));
    }

    #[test]
    fn keeps_the_assumed_script_when_the_first_page_fails() {
        let first_page = FirstPage::<String>::new(None);
        let read = read_pages(
            3,
            &first_page,
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
        assert_eq!(first_page.learned(), None);
    }

    #[test]
    fn gives_up_the_pages_left_when_the_output_stops() {
        let first_page = FirstPage::<String>::new(None);
        let started = AtomicUsize::new(0);
        let pages = 100 * crate::process::processors();
        let first = read_pages(
            pages,
            &first_page,
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
