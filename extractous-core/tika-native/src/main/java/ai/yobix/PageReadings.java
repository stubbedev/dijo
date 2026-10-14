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
