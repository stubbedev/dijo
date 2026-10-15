import java.io.IOException;
import java.io.PrintWriter;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.LinkedHashMap;
import java.util.Map;
import java.util.TreeMap;

import org.apache.fontbox.cff.CFFExpertCharset;
import org.apache.fontbox.cff.CFFExpertEncoding;
import org.apache.fontbox.cff.CFFExpertSubsetCharset;
import org.apache.fontbox.cff.CFFISOAdobeCharset;
import org.apache.fontbox.cff.CFFStandardEncoding;
import org.apache.fontbox.cff.CFFStandardString;
import org.apache.fontbox.encoding.Encoding;

/**
 * Writes FontBox's predefined CFF data as the Rust port reads it: the 391 standard strings, the
 * glyph ids, SIDs and names of the predefined charsets, and the codes and names of the
 * predefined encodings.
 * <p>
 * Usage: {@code java -cp <fontbox jars> CffTables.java <output.tsv>}
 */
public final class CffTables {

    public static void main(String[] args) throws IOException {
        try (PrintWriter out = new PrintWriter(Files.newBufferedWriter(Path.of(args[0]),
                StandardCharsets.UTF_8))) {
            out.print("# string\tsid\tname; charset\tcharset\tgid\tsid\tname; "
                    + "encoding\tencoding\tcode\tname\n");
            for (int sid = 0; sid <= 390; sid++) {
                out.print("string\t" + sid + "\t" + CFFStandardString.getName(sid) + "\n");
            }
            Map<String, org.apache.fontbox.cff.CFFCharset> charsets = new LinkedHashMap<>();
            charsets.put("ISOAdobe", CFFISOAdobeCharset.getInstance());
            charsets.put("Expert", CFFExpertCharset.getInstance());
            charsets.put("ExpertSubset", CFFExpertSubsetCharset.getInstance());
            for (Map.Entry<String, org.apache.fontbox.cff.CFFCharset> charset
                    : charsets.entrySet()) {
                for (int gid = 0; charset.getValue().getNameForGID(gid) != null; gid++) {
                    out.print("charset\t" + charset.getKey() + "\t" + gid + "\t"
                            + charset.getValue().getSIDForGID(gid) + "\t"
                            + charset.getValue().getNameForGID(gid) + "\n");
                }
            }
            Map<String, Encoding> encodings = new LinkedHashMap<>();
            encodings.put("Standard", CFFStandardEncoding.getInstance());
            encodings.put("Expert", CFFExpertEncoding.getInstance());
            for (Map.Entry<String, Encoding> encoding : encodings.entrySet()) {
                for (Map.Entry<Integer, String> code : new TreeMap<>(
                        encoding.getValue().getCodeToNameMap()).entrySet()) {
                    out.print("encoding\t" + encoding.getKey() + "\t" + code.getKey() + "\t"
                            + code.getValue() + "\n");
                }
            }
        }
    }
}
