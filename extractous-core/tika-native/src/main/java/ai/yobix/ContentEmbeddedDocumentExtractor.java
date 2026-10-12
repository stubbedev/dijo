package ai.yobix;

import org.apache.tika.extractor.ParsingEmbeddedDocumentExtractor;
import org.apache.tika.metadata.Metadata;
import org.apache.tika.metadata.TikaCoreProperties;
import org.apache.tika.parser.ParseContext;

/**
 * Parses the documents embedded in a document, except the thumbnails Tika marks, such as the
 * preview an Office or OpenDocument file carries or an e-book's cover: a thumbnail is a picture
 * of the document's own front page, so OCR would only add a garbled copy of text the document
 * or its metadata already holds.
 */
final class ContentEmbeddedDocumentExtractor extends ParsingEmbeddedDocumentExtractor {

    static boolean isThumbnail(Metadata metadata) {
        return TikaCoreProperties.EmbeddedResourceType.THUMBNAIL.toString()
                .equals(metadata.get(TikaCoreProperties.EMBEDDED_RESOURCE_TYPE));
    }

    @Override
    public boolean shouldParseEmbedded(Metadata metadata, ParseContext context) {
        return !isThumbnail(metadata) && super.shouldParseEmbedded(metadata, context);
    }
}
