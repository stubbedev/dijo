package ai.yobix;

import java.io.InputStream;
import org.apache.tika.metadata.Metadata;

public class ReaderResult {

    private final InputStream stream;
    private final byte status;
    private final String errorMessage;
    private final Metadata metadata;

    public ReaderResult(InputStream stream, Metadata metadata) {
        this.stream = stream;
        this.status = 0;
        this.errorMessage = null;
        this.metadata = metadata;
    }

    public ReaderResult(byte status, String errorMessage) {
        this.stream = null;
        this.status = status;
        this.errorMessage = errorMessage;
        this.metadata = null;
    }

    /**
     * Returns the result Reader or null if there is an error
     * @return Reader reader
     */
    public InputStream getStream() {
        return stream;
    }

    public boolean isError() {
        return status != 0;
    }

    /**
     * Returns the tika metadata or null if there is an error
     * @return tika metadata
     */
    public Metadata getMetadata() {
        return metadata;
    }

    /**
     * Returns the status of the call
     * @return
     * 0: OK
     * 1: IOException
     */
    public byte getStatus() {
        return status;
    }

    /**
     * Returns the error message in case of error
     * @return  String representing the error message or
     * null if there is no error
     */
    public String getErrorMessage() {
        return errorMessage;
    }

    public String toString() {
        return "status:" + this.status + " error: " + this.errorMessage + " stream: " + this.stream;
    }
}
