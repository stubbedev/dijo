package ai.yobix;

import org.apache.tika.exception.EncryptedDocumentException;
import org.apache.tika.metadata.Metadata;
import org.apache.tika.mime.MediaType;
import org.apache.tika.parser.ParseContext;
import org.apache.tika.parser.Parser;
import org.xml.sax.ContentHandler;

import java.io.InputStream;
import java.util.Set;

/**
 * Reports OpenPGP-encrypted data as encrypted. Tika has no OpenPGP parser, so the data was
 * returned as an empty document, or, when ASCII-armored, as its ciphertext.
 */
final class OpenPgpParser implements Parser {

    static final MediaType PGP_ENCRYPTED = MediaType.application("pgp-encrypted");

    @Override
    public Set<MediaType> getSupportedTypes(ParseContext context) {
        return Set.of(PGP_ENCRYPTED);
    }

    @Override
    public void parse(InputStream stream, ContentHandler handler, Metadata metadata,
                      ParseContext context) throws EncryptedDocumentException {
        throw new EncryptedDocumentException(
                "OpenPGP-encrypted data; decrypting it needs the recipient's private key");
    }
}
