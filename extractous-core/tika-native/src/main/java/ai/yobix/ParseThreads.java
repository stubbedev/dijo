package ai.yobix;

import java.util.ArrayDeque;
import java.util.concurrent.SynchronousQueue;
import java.util.concurrent.TimeUnit;

final class ParseThreads {

    private static final Object LOCK = new Object();
    private static final ArrayDeque<Worker> IDLE = new ArrayDeque<>();
    private static int running;

    private ParseThreads() {
    }

    static void run(Runnable work, Runnable afterIdle) {
        Worker worker;
        synchronized (LOCK) {
            worker = IDLE.pollFirst();
            running++;
        }
        if (worker == null) {
            worker = new Worker();
            worker.start();
        }
        worker.assign(new Job(work, afterIdle));
    }

    /**
     * Waits until no parse is running, or for at most timeoutMillis, and tells whether none is.
     * A parse whose reader stopped reading is cancelled but still winds down, killing any
     * tesseract it started; a process should wait for that before it exits.
     */
    static boolean awaitIdle(long timeoutMillis) throws InterruptedException {
        final long deadline = System.nanoTime() + TimeUnit.MILLISECONDS.toNanos(timeoutMillis);
        synchronized (LOCK) {
            while (running > 0) {
                final long remaining = deadline - System.nanoTime();
                if (remaining <= 0) {
                    return false;
                }
                TimeUnit.NANOSECONDS.timedWait(LOCK, remaining);
            }
            return true;
        }
    }

    private record Job(Runnable work, Runnable afterIdle) {
    }

    private static final class Worker extends Thread {

        private final SynchronousQueue<Job> jobs = new SynchronousQueue<>();

        Worker() {
            super("Apache Tika");
            setDaemon(true);
        }

        void assign(Job job) {
            try {
                jobs.put(job);
            } catch (InterruptedException e) {
                Thread.currentThread().interrupt();
                throw new IllegalStateException("Interrupted while starting a parse", e);
            }
        }

        @Override
        public void run() {
            while (true) {
                final Job job;
                try {
                    job = jobs.take();
                } catch (InterruptedException e) {
                    return;
                }
                try {
                    job.work().run();
                } finally {
                    synchronized (LOCK) {
                        IDLE.addFirst(this);
                        running--;
                        LOCK.notifyAll();
                    }
                    job.afterIdle().run();
                }
            }
        }
    }
}
