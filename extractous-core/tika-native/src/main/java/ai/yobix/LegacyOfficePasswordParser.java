package ai.yobix;

import org.apache.poi.hssf.record.crypto.Biff8EncryptionKey;
import org.apache.tika.exception.TikaException;
import org.apache.tika.metadata.Metadata;
import org.apache.tika.parser.ParseContext;
import org.apache.tika.parser.Parser;
import org.apache.tika.parser.ParserDecorator;
import org.xml.sax.ContentHandler;
import org.xml.sax.SAXException;

import java.io.IOException;
import java.io.InputStream;

/**
 * Parses with a password for the legacy binary Office formats too. POI decrypts Word 97 and
 * PowerPoint 97 documents with the password in a thread-local rather than asking Tika's
 * PasswordProvider, so it is set for the duration of the parse, on the parsing thread.
 * <p>
 * A password is only ever passed after the document failed to open without one, so a parse that
 * succeeds with it decrypted the document.
 */
final class LegacyOfficePasswordParser extends ParserDecorator {

    private final String password;

    LegacyOfficePasswordParser(Parser parser, String password) {
        super(parser);
        this.password = password;
    }

    @Override
    public void parse(InputStream stream, ContentHandler handler, Metadata metadata,
                      ParseContext context) throws IOException, SAXException, TikaException {
        Biff8EncryptionKey.setCurrentUserPassword(password);
        try {
            super.parse(stream, handler, metadata, context);
            metadata.set(TikaNativeMain.PASSWORD_USED, "true");
        } finally {
            Biff8EncryptionKey.setCurrentUserPassword(null);
        }
    }
}
