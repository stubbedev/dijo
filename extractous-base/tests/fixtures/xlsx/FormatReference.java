import java.io.FileOutputStream;
import java.io.IOException;
import java.io.OutputStreamWriter;
import java.io.PrintWriter;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.List;
import java.util.zip.GZIPOutputStream;

import org.apache.poi.ss.usermodel.BuiltinFormats;
import org.apache.poi.ss.usermodel.DataFormatter;
import org.apache.tika.parser.microsoft.TikaExcelDataFormatter;

/**
 * Writes what Tika's Excel extractor makes of numbers in number formats, for the Rust port of
 * POI's DataFormatter to be tested against: {@code formats.tsv.gz}, a group, a format index, a
 * format string, a double's bits and the text {@code formatRawCellContents} gives, or
 * {@code !thrown} where it throws, which Tika's sheet handler catches.
 * <p>
 * Each group is formatted by one {@link TikaExcelDataFormatter}, in order, as one workbook's
 * cells are: the formatter caches the format it makes of a format string for the cells after.
 * Run it with {@code -Duser.language=en -Duser.country=US -Duser.timezone=Europe/Copenhagen}.
 * <p>
 * Usage: {@code java -cp <POI, Tika's microsoft module and their dependencies>
 * FormatReference.java <directory>}, the directory holding {@code formats.txt}, the custom
 * formats to test, one per line, backslashes and tabs escaped.
 */
public class FormatReference {

    private static final double[] VALUES = {
        0, 1, -1, 0.5, 2.5, -2.5, 1.25, 0.1 + 0.2, 1.0 / 3, 2.0 / 3, 0.001, 0.004, -0.004,
        12.345, 99.995, 0.045, 100.5, 1234.5678, -1234.5678, 1234567.891, 123456789,
        -123456789.25, 1e-10, 1.5e-16, 7.0e-4, 1e15, 1000000000000000.25, 9.99999999999e14,
        1e16, 1e20, 1.23456789012345e21, 1e300, 5e-324, 3.14159265358979, 59, 60, 61, 36526,
        45000, 45000.75, 45000.5000115, 0.999988426, 0.0000115740741, 2958465.99999, -45000.25,
        86399.99999, 0.333333333333, 1.999999999999, 12345678901234.5, 0.30000000000000004,
        1e23, 2e23, 1e-5, 1e22, 123456.7890123456, 0.000123456789012345,
    };

    private static final String[] SYNTHETIC = {
        "0", "0.00", "#,##0", "#,##0.00", "0%", "0.00%", "0.00E+00", "##0.0E+0", "0.0E-0",
        "0.00E+000", "# ?/?", "# ??/??", "?/?", "#/8", "# #/100", "0/0", "# ?/2", "??/??",
        "m/d/yy", "d-mmm-yy", "mmmm d, yyyy", "dddd, mmmm dd, yyyy", "h:mm AM/PM",
        "h:mm:ss a/p", "hh:mm:ss.000", "mm:ss.0", "[h]:mm:ss", "[mm]:ss", "[s]",
        "yyyy-mm-dd hh:mm", "yyyy\\-mm\\-dd\\Thh:mm:ss", "mmmmm", "ddd", "yy", "yyyy",
        "@", "General", "GENERAL", "general", "\"Total: \"0.00", "0.00\" kr\"",
        "[$€-407]#,##0.00", "[$-409]h:mm AM/PM", "#,##0,", "#,##0,,\"M\"", "0.0,",
        "[Red]0.00", "[Blue]#,##0;[Red]-#,##0", "0.00;-0.00;\"zero\"", "0;-0;;@",
        "[>=100]\"big\" 0;[<100]\"small\" 0", "[<1]0.000;0.0", "#,##0.00_);(#,##0.00)",
        "_-* #,##0.00 _€_-;-* #,##0.00 _€_-;_-* \"-\"?? _€_-;_-@_-", "000-00-0000",
        "00000-0000", "[<=9999999]###-####;(###) ###-####", "# ##0", "0.00\\%", "0%;-0%",
        "[h]:mm;-[h]:mm", "dd/mm/yyyy;@", "dd.mm.yyyy hh:mm", "0 \"m²\"",
        "#,##0.00 [$USD]", "[$¥-411]#,##0", "#?", "??0.0??", "0.##", "#.#", ".00", "#",
        "0000", "0.0%;[Red]-0.0%", "[$-F800]dddd, mmmm dd, yyyy", "[$-F400]h:mm:ss AM/PM",
        "0.00;[Red]0.00", "#,##0.00;[Red]-#,##0.00;0.00", "0.000000_ ", "[Color10]0.00",
        "mm:ss;@", "h\" h \"mm\" min\"", "0\" \"0/0", "#,###", "##,##0.0#", "0.0#;(0.0#)",
        "0.00E+00;-0.00E+00", "[>1000]#,##0,\"K\";0", "\"$\"#,##0.00", "0.00 \\€",
        "d/m/yyyy h:mm;@", "yyyy/mm/dd", "mmm-yy", "#,##0.0000", "000.000", "00.0%",
        "0.0E+00", "# ?/4", "0.00;;", ";;;", "\\-0", "0\\.0", "*-0", "_(0_)", "[ss].00",
        "hh:mm:ss.00", "[$$-409]#,##0.00", "#\\ ##0", "0;0;0;@\" text\"", "[<0]\"neg\";0",
    };

    public static void main(String[] args) throws IOException {
        Path directory = Path.of(args[0]);
        List<String> custom = new ArrayList<>();
        for (String line : Files.readAllLines(directory.resolve("formats.txt"), StandardCharsets.UTF_8)) {
            custom.add(unescape(line));
        }
        try (PrintWriter out = new PrintWriter(new OutputStreamWriter(new GZIPOutputStream(
                new FileOutputStream(directory.resolve("formats.tsv.gz").toFile())),
                StandardCharsets.UTF_8))) {
            int group = 0;
            for (int index = 0; index < BuiltinFormats.getAll().length; index++) {
                write(out, group++, index, BuiltinFormats.getBuiltinFormat(index), VALUES);
            }
            List<String> formats = new ArrayList<>(custom);
            for (String format : SYNTHETIC) {
                formats.add(format);
            }
            for (String format : formats) {
                write(out, group++, 164, format, VALUES);
            }
            write(out, group++, 164, "yyyy-mm-dd", new double[] {-1, 45000.5, -2});
            write(out, group++, 14, "m/d/yy", new double[] {-1, 45000.5, -2});
            write(out, group++, 164, "[h]:mm:ss", new double[] {1.5, 45000.5, -0.25});
        }
    }

    private static void write(PrintWriter out, int group, int index, String format,
            double[] values) {
        DataFormatter formatter = new TikaExcelDataFormatter();
        for (double value : values) {
            String result;
            try {
                result = formatter.formatRawCellContents(value, index, format);
            } catch (Exception e) {
                result = "!thrown";
            }
            out.print(group + "\t" + index + "\t" + escape(format) + "\t"
                    + Long.toHexString(Double.doubleToRawLongBits(value)) + "\t" + escape(result)
                    + "\n");
        }
    }

    private static String escape(String text) {
        return text.replace("\\", "\\\\").replace("\t", "\\t").replace("\n", "\\n")
                .replace("\r", "\\r");
    }

    private static String unescape(String text) {
        StringBuilder out = new StringBuilder();
        for (int i = 0; i < text.length(); i++) {
            char c = text.charAt(i);
            if (c == '\\' && i + 1 < text.length()) {
                char next = text.charAt(++i);
                out.append(next == 't' ? '\t' : next == 'n' ? '\n' : next == 'r' ? '\r' : next);
            } else {
                out.append(c);
            }
        }
        return out.toString();
    }
}
