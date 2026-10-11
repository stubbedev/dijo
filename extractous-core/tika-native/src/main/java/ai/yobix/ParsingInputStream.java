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
import java.nio.ByteBuffer;
import java.nio.charset.Charset;
import java.util.Arrays;
import java.util.concurrent.BlockingQueue;
import java.util.concurrent.LinkedBlockingQueue;
import java.util.concurrent.Semaphore;
import java.util.concurrent.TimeUnit;

public class ParsingInputStream extends InputStream {

    private static final int CHUNK_SIZE = 64 * 1024;
    private static final int QUEUED_CHUNKS = 4;
    private static final byte[] END = new byte[0];

    private final BlockingQueue<byte[]> chunks = new LinkedBlockingQueue<>();
    private final Semaphore space = new Semaphore(QUEUED_CHUNKS);
    private final ChunkOutputStream output = new ChunkOutputStream();
    private final Metadata metadata;
    private volatile boolean closed;
    private volatile Throwable failure;
    private byte[] current;
    private int position;

    public ParsingInputStream(Parser parser, InputStream stream, Metadata metadata,
                              ParseContext context, boolean outputXml, Charset charset)
            throws IOException {
        this.metadata = metadata;
        ParseThreads.run(() -> parse(parser, stream, metadata, context, outputXml, charset),
                () -> chunks.add(END));

        if (!nextChunk()) {
            rethrowFailure();
        }
    }

    private void parse(Parser parser, InputStream stream, Metadata metadata, ParseContext context,
                       boolean outputXml, Charset charset) {
        try (stream) {
            final ContentHandler handler = outputXml
                    ? new ToXMLContentHandler(output, charset.name())
                    : new BodyContentHandler(new OutputStreamWriter(output, charset));
            parser.parse(stream, handler, metadata, context);
            output.sendRemainder();
        } catch (Throwable t) {
            failure = t;
        }
    }

    private boolean nextChunk() throws IOException {
        if (current == END) {
            return false;
        }
        try {
            current = chunks.take();
        } catch (InterruptedException e) {
            Thread.currentThread().interrupt();
            throw new InterruptedIOException("Interrupted while waiting for parsed text");
        }
        position = 0;
        if (current == END) {
            return false;
        }
        space.release();
        return true;
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
        if (position == current.length && !nextChunk()) {
            rethrowFailure();
            return -1;
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
        if (position == current.length && !nextChunk()) {
            rethrowFailure();
            return -1;
        }
        final int count = Math.min(target.remaining(), current.length - position);
        target.put(current, position, count);
        position += count;
        return count;
    }

    public Metadata getMetadata() {
        return metadata;
    }

    @Override
    public void close() {
        closed = true;
    }

    private void put(byte[] chunk) throws IOException {
        try {
            while (!space.tryAcquire(100, TimeUnit.MILLISECONDS)) {
                if (closed) {
                    throw new IOException("The text stream was closed before parsing finished");
                }
            }
        } catch (InterruptedException e) {
            Thread.currentThread().interrupt();
            throw new InterruptedIOException("Interrupted while handing over parsed text");
        }
        chunks.add(chunk);
    }

    private final class ChunkOutputStream extends OutputStream {

        private byte[] buffer;
        private int count;

        @Override
        public void write(int value) throws IOException {
            writableBuffer()[count++] = (byte) value;
        }

        @Override
        public void write(byte[] bytes, int offset, int length) throws IOException {
            while (length > 0) {
                final byte[] target = writableBuffer();
                final int copied = Math.min(length, target.length - count);
                System.arraycopy(bytes, offset, target, count, copied);
                count += copied;
                offset += copied;
                length -= copied;
            }
        }

        private byte[] writableBuffer() throws IOException {
            if (buffer != null && count == buffer.length) {
                send();
            }
            if (buffer == null) {
                buffer = new byte[CHUNK_SIZE];
            }
            return buffer;
        }

        private void send() throws IOException {
            final byte[] full = count == buffer.length ? buffer : Arrays.copyOf(buffer, count);
            buffer = null;
            count = 0;
            put(full);
        }

        void sendRemainder() throws IOException {
            if (count > 0) {
                send();
            }
        }
    }
}
