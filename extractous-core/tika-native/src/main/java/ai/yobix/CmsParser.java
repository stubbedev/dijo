package ai.yobix;

import org.apache.tika.exception.EncryptedDocumentException;
import org.apache.tika.exception.TikaException;
import org.apache.tika.io.TikaInputStream;
import org.apache.tika.metadata.Metadata;
import org.apache.tika.mime.MediaType;
import org.apache.tika.parser.ParseContext;
import org.apache.tika.parser.Parser;
import org.apache.tika.parser.crypto.Pkcs7Parser;
import org.apache.tika.sax.XHTMLContentHandler;
import org.bouncycastle.asn1.ASN1ObjectIdentifier;
import org.bouncycastle.asn1.cms.CMSObjectIdentifiers;
import org.xml.sax.ContentHandler;
import org.xml.sax.SAXException;

import java.io.IOException;
import java.io.InputStream;
import java.util.Arrays;
import java.util.Map;
import java.util.Set;

/**
 * Parses S/MIME and other CMS (PKCS #7) data. Tika's own parser only understands signed data:
 * encrypted data made it fail with an internal error, and a detached signature, which has no
 * content of its own, with "no signed data to parse".
 */
final class CmsParser implements Parser {

    static final MediaType PKCS7_MIME = MediaType.application("pkcs7-mime");
    static final MediaType PKCS7_SIGNATURE = MediaType.application("pkcs7-signature");

    private static final int HEADER_BYTES = 64;
    private static final byte SEQUENCE = 0x30;
    private static final byte OBJECT_IDENTIFIER = 0x06;
    private static final Map<ASN1ObjectIdentifier, String> ENCRYPTED = Map.of(
            CMSObjectIdentifiers.envelopedData, "EnvelopedData",
            CMSObjectIdentifiers.authEnvelopedData, "AuthEnvelopedData",
            CMSObjectIdentifiers.encryptedData, "EncryptedData");

    private final Parser signedData = new Pkcs7Parser();

    @Override
    public Set<MediaType> getSupportedTypes(ParseContext context) {
        return Set.of(PKCS7_MIME, PKCS7_SIGNATURE);
    }

    @Override
    public void parse(InputStream stream, ContentHandler handler, Metadata metadata,
                      ParseContext context) throws IOException, SAXException, TikaException {
        final TikaInputStream input = TikaInputStream.get(stream);
        input.mark(HEADER_BYTES);
        final byte[] header = input.readNBytes(HEADER_BYTES);
        input.reset();
        final ASN1ObjectIdentifier contentType = contentType(header);
        final String encryption = contentType == null ? null : ENCRYPTED.get(contentType);
        if (encryption != null) {
            throw new EncryptedDocumentException("S/MIME (CMS " + encryption
                    + ") data; decrypting it needs the recipient's private key");
        }
        final MediaType type = MediaType.parse(metadata.get(Metadata.CONTENT_TYPE));
        if (type != null && type.getBaseType().equals(PKCS7_SIGNATURE)) {
            final XHTMLContentHandler xhtml = new XHTMLContentHandler(handler, metadata);
            xhtml.startDocument();
            xhtml.endDocument();
            return;
        }
        signedData.parse(input, handler, metadata, context);
    }

    /**
     * The content type of a DER or BER encoded ContentInfo: the object identifier that opens its
     * outer SEQUENCE, or null when the header is not one.
     */
    static ASN1ObjectIdentifier contentType(byte[] header) {
        if (header.length < 2 || header[0] != SEQUENCE) {
            return null;
        }
        int index = 1;
        final int length = header[index++] & 0xff;
        if (length > 0x80) {
            index += length & 0x7f;
        }
        if (index + 2 > header.length || header[index] != OBJECT_IDENTIFIER) {
            return null;
        }
        final int oidLength = header[index + 1] & 0xff;
        final int end = index + 2 + oidLength;
        if (oidLength >= 0x80 || end > header.length) {
            return null;
        }
        return ASN1ObjectIdentifier.getInstance(Arrays.copyOfRange(header, index, end));
    }
}
