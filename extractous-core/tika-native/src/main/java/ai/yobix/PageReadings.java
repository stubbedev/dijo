package ai.yobix;

import org.xml.sax.SAXException;

import java.util.ArrayDeque;
import java.util.concurrent.Callable;
import java.util.concurrent.CancellationException;
import java.util.concurrent.CompletableFuture;
import java.util.concurrent.ExecutionException;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;
import java.util.concurrent.Future;

/**
 * The threads pages of a document are read by OCR on while the parse that hands them over goes
 * on: as many as the processors this process may use ({@link ReadingThreads}), shared by every
 * extraction in it, so that the pages of a scanned document are read side by side.
 */
final class PageReadings {

    static final int THREADS = ReadingThreads.processors();

    private static final ExecutorService POOL = Executors.newFixedThreadPool(THREADS, reading -> {
        final Thread thread = new Thread(reading, "extractous page reading");
        thread.setDaemon(true);
        return thread;
    });

    private PageReadings() {
    }

    /** What a page passes on once it is read, on the thread that parses the document. */
    @FunctionalInterface
    interface Output {
        void pass() throws SAXException;
    }

    /** A reading of a page that is first read in a given script. */
    @FunctionalInterface
    interface InScript<T> {
        T read(AssumedScript script) throws Exception;
    }

    /**
     * The script the pages of a document after its first are first read in: the one assumed
     * before the first page, unless reading it teaches another. A later page read before the
     * first is done is read in the assumed script, then read again if the first teaches another,
     * so that every page reads as it would after the first, whichever finishes first.
     */
    static final class FirstPage {
        private final AssumedScript assumed;
        private final CompletableFuture<String> learned = new CompletableFuture<>();

        FirstPage(AssumedScript assumed) {
            this.assumed = assumed;
        }

        AssumedScript assumed() {
            return assumed;
        }

        /**
         * Settles the script of the later pages: the one the first page learned, or, when it is
         * null, the one assumed. Only the first call counts, so that a page that could not be
         * read, or a parse that is given up, settles it as assumed.
         */
        void settle(String script) {
            learned.complete(script);
        }

        /** The script the first page learned, or null when it learned none or is not settled. */
        String learned() {
            return learned.getNow(null);
        }

        /**
         * Reads a later page in the script the first page settles; a guess in the assumed script
         * that fails counts only when the first page settles on that script.
         */
        <T> T later(InScript<T> reading) throws Exception {
            if (learned.isDone()) {
                return reading.read(settled());
            }
            T guess = null;
            Exception failure = null;
            try {
                guess = reading.read(assumed);
            } catch (Exception e) {
                failure = e;
            }
            final AssumedScript script = settled();
            if (!script.equals(assumed)) {
                return reading.read(script);
            }
            if (failure != null) {
                throw failure;
            }
            return guess;
        }

        private AssumedScript settled() throws InterruptedException, ExecutionException {
            final String script = learned.get();
            return script != null ? new AssumedScript(script) : assumed;
        }
    }

    /**
     * The outputs of a document's pages, in document order: each is passed on as soon as it and
     * every one before it are ready, and the parse waits for the oldest while as many are being
     * read as there are threads.
     */
    static final class InOrder {
        private final ArrayDeque<Future<Output>> pending = new ArrayDeque<>();

        boolean isEmpty() {
            return pending.isEmpty();
        }

        /** Queues an output that is ready now. */
        void ready(Output output) throws SAXException {
            queue(CompletableFuture.completedFuture(output));
        }

        /** Queues an output that a reading on the threads makes. */
        void read(Callable<Output> reading) throws SAXException {
            queue(POOL.submit(reading));
        }

        private void queue(Future<Output> output) throws SAXException {
            pending.add(output);
            while (!pending.isEmpty()
                    && (pending.peekFirst().isDone() || pending.size() > THREADS)) {
                pass(pending.removeFirst());
            }
        }

        /** Passes on every queued output, waiting for each in turn. */
        void passAll() throws SAXException {
            while (!pending.isEmpty()) {
                pass(pending.removeFirst());
            }
        }

        private static void pass(Future<Output> output) throws SAXException {
            try {
                output.get().pass();
            } catch (InterruptedException e) {
                Thread.currentThread().interrupt();
                throw new SAXException("Interrupted while pages were read by OCR", e);
            } catch (ExecutionException e) {
                throw new SAXException("Reading a page by OCR failed", e);
            }
        }

        /**
         * Gives up what is still queued when the parse ends early: readings not started are not
         * run, and those running are waited for, so that what they read from outlives them.
         */
        void abandon() {
            for (final Future<Output> output : pending) {
                output.cancel(false);
            }
            for (final Future<Output> output : pending) {
                try {
                    output.get();
                } catch (InterruptedException e) {
                    Thread.currentThread().interrupt();
                    return;
                } catch (ExecutionException | CancellationException ignored) {
                    continue;
                }
            }
            pending.clear();
        }
    }
}
