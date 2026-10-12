package ai.yobix;

/**
 * A document embedded in the one being parsed, copied to its own file instead of being parsed
 * inline, so the caller can extract it as a document in its own right.
 */
public class SpooledEmbedded {
    private final String name;
    private final String contentType;
    private final String path;
    private final long size;
    private final boolean truncated;

    public SpooledEmbedded(String name, String contentType, String path, long size, boolean truncated) {
        this.name = name;
        this.contentType = contentType;
        this.path = path;
        this.size = size;
        this.truncated = truncated;
    }

    public String getName() {
        return name;
    }

    public String getContentType() {
        return contentType;
    }

    public String getPath() {
        return path;
    }

    public long getSize() {
        return size;
    }

    /** True when the document was larger than the byte budget left and only its start was kept. */
    public boolean isTruncated() {
        return truncated;
    }
}
