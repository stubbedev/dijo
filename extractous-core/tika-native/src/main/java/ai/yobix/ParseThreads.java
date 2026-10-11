package ai.yobix;

import java.util.ArrayDeque;
import java.util.concurrent.SynchronousQueue;

final class ParseThreads {

    private static final Object LOCK = new Object();
    private static final ArrayDeque<Worker> IDLE = new ArrayDeque<>();

    private ParseThreads() {
    }

    static void run(Runnable work, Runnable afterIdle) {
        Worker worker;
        synchronized (LOCK) {
            worker = IDLE.pollFirst();
        }
        if (worker == null) {
            worker = new Worker();
            worker.start();
        }
        worker.assign(new Job(work, afterIdle));
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
                    }
                    job.afterIdle().run();
                }
            }
        }
    }
}
