//! Our Java `PageReadings`: the pages of a document read by OCR side by side, on as many threads
//! as the process may use processors, while the pages after them are decoded or rendered. The
//! first page is read as any image is; the rest are first read in the script it leaves the
//! extraction assuming, so that every page reads as it would after the first, whichever
//! finishes first.

use std::sync::mpsc;
use std::sync::{Condvar, Mutex};

/// The script the pages of a document after its first are first read in: the one assumed
/// before the first page, unless reading it teaches another.
pub(crate) struct FirstPage {
    assumed: Option<String>,
    /// The script the first page learned, once it is settled: none when it learned none.
    learned: Mutex<Option<Option<String>>>,
    settled: Condvar,
}

impl FirstPage {
    pub(crate) fn new(assumed: Option<String>) -> Self {
        Self {
            assumed,
            learned: Mutex::new(None),
            settled: Condvar::new(),
        }
    }

    /// The script assumed before the first page.
    pub(crate) fn assumed(&self) -> Option<&str> {
        self.assumed.as_deref()
    }

    /// Settles the script of the later pages: the one the first page learned, or, when it is
    /// none, the one assumed. Only the first call counts, so that a page that could not be read
    /// settles it as assumed.
    pub(crate) fn settle(&self, learned: Option<String>) {
        let mut settled = self.learned.lock().unwrap_or_else(|e| e.into_inner());
        if settled.is_none() {
            *settled = Some(learned);
            self.settled.notify_all();
        }
    }

    /// The script the first page learned, once it is settled.
    pub(crate) fn learned(&self) -> Option<String> {
        self.learned
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .flatten()
    }

    /// The script the later pages are read in, waiting until it is settled.
    fn settled(&self) -> Option<String> {
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

    /// Reads a later page in the script the first page settles. A page read before it is
    /// settled is read in the assumed script, and read again if the first page teaches
    /// another; a failure of that guess counts only when it stands.
    pub(crate) fn later<T, E>(&self, read: impl Fn(Option<&str>) -> Result<T, E>) -> Result<T, E> {
        if self.is_settled() {
            return read(self.settled().as_deref());
        }
        let guess = read(self.assumed());
        let script = self.settled();
        if script != self.assumed {
            return read(script.as_deref());
        }
        guess
    }
}

/// Settles the first page as assumed when its reading ends without settling it.
struct SettleOnExit<'a>(&'a FirstPage);

impl Drop for SettleOnExit<'_> {
    fn drop(&mut self) {
        self.0.settle(None);
    }
}

/// Reads pages side by side on `threads` threads, each as `read` reads it, as `pages` hands
/// them over in order: the outputs in page order. The first page is read with `first`, which
/// settles the script of the later pages, read with `later`.
pub(crate) fn read_side_by_side<P: Send, T: Send>(
    pages: impl Iterator<Item = P>,
    threads: usize,
    first_page: &FirstPage,
    first: impl FnOnce(P) -> T + Send,
    later: impl Fn(P) -> T + Sync,
) -> Vec<T> {
    let threads = threads.max(1);
    let (queue, queued) = mpsc::sync_channel::<(usize, P)>(0);
    let queued = Mutex::new(queued);
    let (done, outputs) = mpsc::channel::<(usize, T)>();
    let first = Mutex::new(Some(first));
    std::thread::scope(|scope| {
        for _ in 0..threads {
            let done = done.clone();
            let (queued, first, later) = (&queued, &first, &later);
            scope.spawn(move || loop {
                let next = queued.lock().unwrap_or_else(|e| e.into_inner()).recv();
                let Ok((index, page)) = next else {
                    return;
                };
                let output = if index == 0 {
                    let _settle = SettleOnExit(first_page);
                    let read = first
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .take()
                        .expect("the first page is read once");
                    read(page)
                } else {
                    later(page)
                };
                if done.send((index, output)).is_err() {
                    return;
                }
            });
        }
        drop(done);
        let mut handed = 0;
        for (index, page) in pages.enumerate() {
            handed = index + 1;
            if queue.send((index, page)).is_err() {
                break;
            }
        }
        if handed == 0 {
            first_page.settle(None);
        }
        drop(queue);
    });
    let mut read: Vec<(usize, T)> = outputs.into_iter().collect();
    read.sort_by_key(|(index, _)| *index);
    read.into_iter().map(|(_, output)| output).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_later_pages_in_the_script_the_first_learns() {
        let first_page = FirstPage::new(Some("Latin".to_owned()));
        let read = read_side_by_side(
            0..6,
            3,
            &first_page,
            |page| {
                std::thread::sleep(std::time::Duration::from_millis(20));
                first_page.settle(Some("Cyrillic".to_owned()));
                format!("{page}:first")
            },
            |page| {
                first_page
                    .later(|script| Ok::<_, ()>(format!("{page}:{}", script.unwrap_or("-"))))
                    .unwrap()
            },
        );
        assert_eq!(
            read,
            [
                "0:first",
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
    fn keeps_the_assumed_script_when_the_first_page_learns_none() {
        let first_page = FirstPage::new(None);
        let read = read_side_by_side(
            0..3,
            2,
            &first_page,
            |page| page * 10,
            |page| first_page.later(|_| Ok::<_, ()>(page)).unwrap(),
        );
        assert_eq!(read, [0, 1, 2]);
        assert_eq!(first_page.learned(), None);
    }
}
