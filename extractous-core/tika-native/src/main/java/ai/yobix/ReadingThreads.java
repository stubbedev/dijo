package ai.yobix;

import java.util.concurrent.atomic.AtomicInteger;

/**
 * The threads one tesseract reading may run on: the processors shared among the readings running
 * at once, at most the {@value #MAX} tesseract's recognizer divides its work among and at least
 * one. A reading that starts while every processor has a reading runs on one thread, as each
 * reading did before tesseract could divide one.
 *
 * <p>The processors are the machine's, or the share the system property {@value #PROCESSORS}
 * gives a process that shares the machine with other extracting processes. Each of their
 * readings' threads would otherwise count every processor as its own, and the recognizer's
 * threads, which spin while they wait for each other, would starve one another.
 */
final class ReadingThreads implements AutoCloseable {

    private static final int MAX = 4;
    private static final String PROCESSORS = "extractous.processors";
    private static final AtomicInteger RUNNING = new AtomicInteger();

    private final int threads;

    private ReadingThreads(int threads) {
        this.threads = threads;
    }

    /** Counts a reading as running until it is closed, and gives it its threads. */
    static ReadingThreads take() {
        final int running = RUNNING.incrementAndGet();
        return new ReadingThreads(Math.max(1, Math.min(MAX, processors() / running)));
    }

    static int processors() {
        final String share = System.getProperty(PROCESSORS);
        return share == null ? Runtime.getRuntime().availableProcessors() : Integer.parseInt(share);
    }

    int threads() {
        return threads;
    }

    @Override
    public void close() {
        RUNNING.decrementAndGet();
    }
}
