package ai.yobix;

import org.apache.tika.metadata.Metadata;

/**
 * What Tika made of a document embedded in one a native parser reads: the SAX events of its body
 * as {@link EventRecorder} encodes them, its metadata, and, when its parser failed in a way Tika
 * records in the embedding document's metadata, that failure as Tika formats it there. A failure
 * that fails the embedding document too is reported through the status instead, with the events
 * written before it.
 */
public final class EmbeddedEvents {

    private final byte status;
    private final String errorMessage;
    private final byte[] events;
    private final Metadata metadata;
    private final String recordedException;

    private EmbeddedEvents(byte status, String errorMessage, byte[] events, Metadata metadata,
                           String recordedException) {
        this.status = status;
        this.errorMessage = errorMessage;
        this.events = events;
        this.metadata = metadata;
        this.recordedException = recordedException;
    }

    static EmbeddedEvents parsed(byte[] events, Metadata metadata, String recordedException) {
        return new EmbeddedEvents((byte) 0, null, events, metadata, recordedException);
    }

    static EmbeddedEvents failed(byte status, String errorMessage, byte[] events) {
        return new EmbeddedEvents(status, errorMessage, events, null, null);
    }

    public boolean isError() {
        return status != 0;
    }

    public byte getStatus() {
        return status;
    }

    public String getErrorMessage() {
        return errorMessage;
    }

    public byte[] getEvents() {
        return events;
    }

    public Metadata getMetadata() {
        return metadata;
    }

    public String getRecordedException() {
        return recordedException;
    }
}
