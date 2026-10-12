package ai.yobix;

import org.apache.tika.extractor.ParsingEmbeddedDocumentExtractor;
import org.apache.tika.metadata.Metadata;
import org.apache.tika.metadata.TikaCoreProperties;
import org.apache.tika.parser.ParseContext;

/**
 * Parses the documents embedded in a document, except the thumbnails Office and OpenDocument
 * files carry: a thumbnail is a picture of the document's own first page, so OCR would only add
 * a garbled copy of text that is already there.
 */
final class ContentEmbeddedDocumentExtractor extends ParsingEmbeddedDocumentExtractor {

    ContentEmbeddedDocumentExtractor(ParseContext context) {
        super(context);
    }

    static boolean isThumbnail(Metadata metadata) {
        return TikaCoreProperties.EmbeddedResourceType.THUMBNAIL.toString()
                .equals(metadata.get(TikaCoreProperties.EMBEDDED_RESOURCE_TYPE));
    }

    @Override
    public boolean shouldParseEmbedded(Metadata metadata) {
        return !isThumbnail(metadata) && super.shouldParseEmbedded(metadata);
    }
}
