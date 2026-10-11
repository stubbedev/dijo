package ai.yobix;

import java.util.List;
import org.apache.tika.metadata.Metadata;

/**
 * Result class for embedded document extraction operations
 */
public class EmbeddedExtractResult {
    private final byte status;
    private final String errorMessage;
    private final List<EmbeddedDocument> embeddedDocuments;
    private final Metadata metadata;

    public EmbeddedExtractResult(List<EmbeddedDocument> embeddedDocuments, Metadata metadata) {
        this.status = 0;
        this.errorMessage = null;
        this.embeddedDocuments = embeddedDocuments;
        this.metadata = metadata;
    }

    public EmbeddedExtractResult(byte status, String errorMessage) {
        this.status = status;
        this.errorMessage = errorMessage;
        this.embeddedDocuments = null;
        this.metadata = null;
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

    public List<EmbeddedDocument> getEmbeddedDocuments() {
        return embeddedDocuments;
    }

    public Metadata getMetadata() {
        return metadata;
    }

    public static class EmbeddedDocument {
        private final String resourceName;
        private final String contentType;
        private final byte[] content;
        private final String embeddedRelationshipId;

        public EmbeddedDocument(String resourceName, String contentType, byte[] content, String embeddedRelationshipId) {
            this.resourceName = resourceName;
            this.contentType = contentType;
            this.content = content;
            this.embeddedRelationshipId = embeddedRelationshipId;
        }

        public String getResourceName() {
            return resourceName;
        }

        public String getContentType() {
            return contentType;
        }

        public byte[] getContent() {
            return content;
        }

        public String getEmbeddedRelationshipId() {
            return embeddedRelationshipId;
        }
    }
}