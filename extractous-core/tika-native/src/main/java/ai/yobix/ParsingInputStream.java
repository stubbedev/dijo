package ai.yobix;

import org.apache.tika.exception.ZeroByteFileException;
import org.apache.tika.metadata.Metadata;
import org.apache.tika.parser.ParseContext;
import org.apache.tika.parser.Parser;
import org.apache.tika.sax.BodyContentHandler;
import org.apache.tika.sax.ToXMLContentHandler;
import org.xml.sax.ContentHandler;

import java.io.IOException;
import java.io.InputStream;
import java.io.InterruptedIOException;
import java.io.OutputStream;
import java.io.OutputStreamWriter;
import java.io.Writer;
import java.nio.ByteBuffer;
import java.nio.charset.Charset;
import java.util.Arrays;
import java.util.concurrent.BlockingQueue;
import java.util.concurrent.CountDownLatch;
import java.util.concurrent.LinkedBlockingQueue;
import java.util.concurrent.Semaphore;
import java.util.concurrent.TimeUnit;

/**
 * The text of a document, parsed on a worker thread and handed over in chunks. Full chunks flow
 * through a bounded queue; a reader that has waited a moment takes whatever text is buffered,
 * so text arrives promptly and a reader that hits its deadline keeps everything produced so far.
 */
public class ParsingInputStream extends InputStream {

    /** Returned by readInto when the deadline passed before more text arrived. */
    public static final int TIMED_OUT = -2;

    private static final int CHUNK_SIZE = 64 * 1024;
    private static final int QUEUED_CHUNKS = 4;
    private static final long PARTIAL_WAIT_NANOS = TimeUnit.MILLISECONDS.toNanos(50);
    private static final long CLOSE_WAIT_SECONDS = 5;
    private static final byte[] END = new byte[0];

    private enum Wait { CHUNK, END, TIMEOUT }

    private final BlockingQueue<byte[]> chunks = new LinkedBlockingQueue<>();
    private final Semaphore space = new Semaphore(QUEUED_CHUNKS);
    private final ChunkOutputStream output = new ChunkOutputStream();
    private final Metadata metadata;
    private final EmbeddedSpooler spooler;
    private final long deadline;
    private final Object parseLock = new Object();
    private final CountDownLatch parseEnded = new CountDownLatch(1);
    private Thread parseThread;
    private volatile boolean closed;
    private volatile Throwable failure;
    private byte[] current;
    private int position;

    /**
     * @param timeoutMillis how long, from now, reads may wait for text; 0 for no limit
     */
    public ParsingInputStream(Parser parser, InputStream stream, Metadata metadata,
                              ParseContext context, boolean outputXml, Charset charset,
                              long timeoutMillis, EmbeddedSpooler spooler)
            throws IOException {
        this.metadata = metadata;
        this.spooler = spooler;
        this.deadline = timeoutMillis > 0
                ? System.nanoTime() + TimeUnit.MILLISECONDS.toNanos(timeoutMillis)
                : Long.MAX_VALUE;
        ParseThreads.run(() -> parse(parser, stream, metadata, context, outputXml, charset),
                () -> chunks.add(END));

        if (nextChunk() == Wait.END) {
            rethrowFailure();
        }
    }

    private void parse(Parser parser, InputStream stream, Metadata metadata, ParseContext context,
                       boolean outputXml, Charset charset) {
        synchronized (parseLock) {
            if (closed) {
                parseEnded.countDown();
                return;
            }
            parseThread = Thread.currentThread();
        }
        try (stream) {
            final ContentHandler handler = outputXml
                    ? new ToXMLContentHandler(output, charset.name())
                    : new BodyContentHandler(
                            new FlushingWriter(new OutputStreamWriter(output, charset)));
            parser.parse(stream, handler, metadata, context);
            output.sendRemainder();
        } catch (Throwable t) {
            failure = t;
        } finally {
            synchronized (parseLock) {
                parseThread = null;
                Thread.interrupted();
            }
            parseEnded.countDown();
        }
    }

    /**
     * Stops the parse: no more text is wanted. The parsing thread is interrupted, which makes
     * Tika kill a running tesseract, and the interrupt is cleared before the pooled thread takes
     * its next parse.
     */
    private void cancel() {
        synchronized (parseLock) {
            closed = true;
            if (parseThread != null) {
                parseThread.interrupt();
            }
        }
    }

    /**
     * Waits for the next full chunk, taking the buffered partial one after a short wait, until
     * the end of the text or the deadline.
     */
    private Wait nextChunk() throws IOException {
        if (current == END) {
            return Wait.END;
        }
        while (true) {
            final long remaining = deadline - System.nanoTime();
            if (remaining <= 0) {
                return take(output.takePartial(), false) ? Wait.CHUNK : Wait.TIMEOUT;
            }
            final byte[] next;
            try {
                next = chunks.poll(Math.min(remaining, PARTIAL_WAIT_NANOS), TimeUnit.NANOSECONDS);
            } catch (InterruptedException e) {
                Thread.currentThread().interrupt();
                throw new InterruptedIOException("Interrupted while waiting for parsed text");
            }
            if (next == END) {
                current = END;
                return Wait.END;
            }
            if (take(next, true) || take(output.takePartial(), false)) {
                return Wait.CHUNK;
            }
        }
    }

    private boolean take(byte[] chunk, boolean queued) {
        if (chunk == null) {
            return false;
        }
        if (queued) {
            space.release();
        }
        current = chunk;
        position = 0;
        return true;
    }

    private boolean exhausted() {
        return current == null || position == current.length;
    }

    private void rethrowFailure() throws IOException {
        final Throwable t = failure;
        if (t == null || t instanceof ZeroByteFileException) {
            return;
        }
        if (t instanceof IOException) {
            throw (IOException) t;
        }
        throw new IOException("Parsing failed", t);
    }

    public Metadata getMetadata() {
        return metadata;
    }

    /** The embedded documents spooled while parsing; complete once the text has ended. */
    public SpooledEmbedded[] getEmbedded() {
        return spooler == null
                ? new SpooledEmbedded[0]
                : spooler.spooled().toArray(new SpooledEmbedded[0]);
    }

    @Override
    public int read() throws IOException {
        final byte[] single = new byte[1];
        return read(single, 0, 1) == -1 ? -1 : single[0] & 0xff;
    }

    @Override
    public int read(byte[] buffer, int offset, int length) throws IOException {
        if (length == 0) {
            return 0;
        }
        if (exhausted()) {
            switch (nextChunk()) {
                case END -> {
                    rethrowFailure();
                    return -1;
                }
                case TIMEOUT -> {
                    cancel();
                    throw new InterruptedIOException("The extraction exceeded its time limit");
                }
                case CHUNK -> { }
            }
        }
        final int count = Math.min(length, current.length - position);
        System.arraycopy(current, position, buffer, offset, count);
        position += count;
        return count;
    }

    public int readInto(ByteBuffer target) throws IOException {
        if (!target.hasRemaining()) {
            return 0;
        }
        if (exhausted()) {
            switch (nextChunk()) {
                case END -> {
                    rethrowFailure();
                    return -1;
                }
                case TIMEOUT -> {
                    cancel();
                    return TIMED_OUT;
                }
                case CHUNK -> { }
            }
        }
        final int count = Math.min(target.remaining(), current.length - position);
        target.put(current, position, count);
        position += count;
        return count;
    }

    /**
     * Stops the parse and waits a little for it to end, so the processes it started (tesseract)
     * are gone before the caller moves on or exits.
     */
    @Override
    public void close() throws IOException {
        cancel();
        try {
            parseEnded.await(CLOSE_WAIT_SECONDS, TimeUnit.SECONDS);
        } catch (InterruptedException e) {
            Thread.currentThread().interrupt();
            throw new InterruptedIOException("Interrupted while waiting for the parse to stop");
        }
    }

    private void ensureOpen() throws IOException {
        if (closed) {
            throw new IOException("The text stream was closed before parsing finished");
        }
    }

    private void put(byte[] chunk) throws IOException {
        try {
            do {
                ensureOpen();
            } while (!space.tryAcquire(100, TimeUnit.MILLISECONDS));
        } catch (InterruptedException e) {
            Thread.currentThread().interrupt();
            throw new InterruptedIOException("Interrupted while handing over parsed text");
        }
        chunks.add(chunk);
    }

    /** Pushes every write through to the byte stream, so no encoded text waits in a buffer. */
    private static final class FlushingWriter extends Writer {

        private final Writer delegate;

        FlushingWriter(Writer delegate) {
            this.delegate = delegate;
        }

        @Override
        public void write(char[] characters, int offset, int length) throws IOException {
            delegate.write(characters, offset, length);
            delegate.flush();
        }

        @Override
        public void flush() throws IOException {
            delegate.flush();
        }

        @Override
        public void close() throws IOException {
            delegate.close();
        }
    }

    /**
     * Collects the parser's bytes into chunks. A full chunk is queued outside the lock; it is
     * marked in transit until it is in the queue, so the reader never takes text written after it
     * first.
     */
    private final class ChunkOutputStream extends OutputStream {

        private final Object lock = new Object();
        private byte[] buffer;
        private int count;
        private boolean inTransit;

        @Override
        public void write(int value) throws IOException {
            write(new byte[] {(byte) value}, 0, 1);
        }

        @Override
        public void write(byte[] bytes, int offset, int length) throws IOException {
            ensureOpen();
            while (length > 0) {
                byte[] full = null;
                synchronized (lock) {
                    if (buffer == null) {
                        buffer = new byte[CHUNK_SIZE];
                    }
                    final int copied = Math.min(length, buffer.length - count);
                    System.arraycopy(bytes, offset, buffer, count, copied);
                    count += copied;
                    offset += copied;
                    length -= copied;
                    if (count == buffer.length) {
                        full = buffer;
                        buffer = null;
                        count = 0;
                        inTransit = true;
                    }
                }
                if (full != null) {
                    try {
                        put(full);
                    } finally {
                        synchronized (lock) {
                            inTransit = false;
                        }
                    }
                }
            }
        }

        void sendRemainder() throws IOException {
            final byte[] remainder;
            synchronized (lock) {
                remainder = drain();
            }
            if (remainder != null) {
                put(remainder);
            }
        }

        /**
         * The buffered text, for a reader that has waited long enough, but only when nothing
         * written before it is still on its way: no full chunk in transit or in the queue.
         */
        byte[] takePartial() {
            synchronized (lock) {
                return inTransit || !chunks.isEmpty() ? null : drain();
            }
        }

        private byte[] drain() {
            if (count == 0) {
                return null;
            }
            final byte[] partial = Arrays.copyOf(buffer, count);
            count = 0;
            return partial;
        }
    }
}
