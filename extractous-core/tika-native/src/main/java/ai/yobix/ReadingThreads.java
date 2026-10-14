package ai.yobix;

import java.util.concurrent.atomic.AtomicInteger;

/**
 * The threads one tesseract reading may run on: the processors shared among the readings running
 * at once, at most the {@value #MAX} tesseract's recognizer divides its work among and at least
 * one. A reading that starts while every processor has a reading runs on one thread, as each
 * reading did before tesseract could divide one.
 */
final class ReadingThreads implements AutoCloseable {

    private static final int MAX = 4;
    private static final AtomicInteger RUNNING = new AtomicInteger();

    private final int threads;

    private ReadingThreads(int threads) {
        this.threads = threads;
    }

    /** Counts a reading as running until it is closed, and gives it its threads. */
    static ReadingThreads take() {
        final int running = RUNNING.incrementAndGet();
        final int processors = Runtime.getRuntime().availableProcessors();
        return new ReadingThreads(Math.max(1, Math.min(MAX, processors / running)));
    }

    int threads() {
        return threads;
    }

    @Override
    public void close() {
        RUNNING.decrementAndGet();
    }
}
