import java.io.IOException;
import java.io.PrintWriter;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.Map;
import java.util.TreeMap;

import org.apache.pdfbox.pdmodel.font.encoding.Encoding;
import org.apache.pdfbox.pdmodel.font.encoding.MacExpertEncoding;
import org.apache.pdfbox.pdmodel.font.encoding.MacOSRomanEncoding;
import org.apache.pdfbox.pdmodel.font.encoding.MacRomanEncoding;
import org.apache.pdfbox.pdmodel.font.encoding.StandardEncoding;
import org.apache.pdfbox.pdmodel.font.encoding.SymbolEncoding;
import org.apache.pdfbox.pdmodel.font.encoding.WinAnsiEncoding;
import org.apache.pdfbox.pdmodel.font.encoding.ZapfDingbatsEncoding;

/**
 * Writes PDFBox's built-in simple font encodings as the Rust port reads them: for each encoding,
 * its code-to-name map and its name-to-code map, which PDFBox builds with first-name-wins rules
 * of its own, so neither is derivable from the other.
 * <p>
 * Usage: {@code java -cp <pdfbox jars> PdfEncodings.java <output.tsv>}
 */
public final class PdfEncodings {

    public static void main(String[] args) throws IOException {
        Map<String, Encoding> encodings = new java.util.LinkedHashMap<>();
        encodings.put("Standard", StandardEncoding.INSTANCE);
        encodings.put("WinAnsi", WinAnsiEncoding.INSTANCE);
        encodings.put("MacRoman", MacRomanEncoding.INSTANCE);
        encodings.put("MacOSRoman", MacOSRomanEncoding.INSTANCE);
        encodings.put("MacExpert", MacExpertEncoding.INSTANCE);
        encodings.put("Symbol", SymbolEncoding.INSTANCE);
        encodings.put("ZapfDingbats", ZapfDingbatsEncoding.INSTANCE);
        try (PrintWriter out = new PrintWriter(Files.newBufferedWriter(Path.of(args[0]),
                StandardCharsets.UTF_8))) {
            out.print("# encoding\tcode\tname: code to name; encoding\tname\tcode: name to code\n");
            for (Map.Entry<String, Encoding> entry : encodings.entrySet()) {
                for (Map.Entry<Integer, String> code : new TreeMap<>(
                        entry.getValue().getCodeToNameMap()).entrySet()) {
                    out.print(entry.getKey() + "\tcode\t" + code.getKey() + "\t" + code.getValue()
                            + "\n");
                }
                for (Map.Entry<String, Integer> name : new TreeMap<>(
                        entry.getValue().getNameToCodeMap()).entrySet()) {
                    out.print(entry.getKey() + "\tname\t" + name.getKey() + "\t" + name.getValue()
                            + "\n");
                }
            }
        }
    }
}
