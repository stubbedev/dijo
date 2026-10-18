import java.io.IOException;
import java.io.PrintWriter;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;

import org.apache.pdfbox.cos.COSArray;
import org.apache.pdfbox.cos.COSDictionary;
import org.apache.pdfbox.cos.COSFloat;
import org.apache.pdfbox.cos.COSName;
import org.apache.pdfbox.pdmodel.graphics.color.PDCalGray;
import org.apache.pdfbox.pdmodel.graphics.color.PDCalRGB;
import org.apache.pdfbox.pdmodel.graphics.color.PDColorSpace;
import org.apache.pdfbox.pdmodel.graphics.color.PDLab;

/**
 * Writes the sRGB colours PDFBox's CIE-based colour spaces give colours on the JVM, through the
 * JDK's CIE XYZ profile, which Tika's native image lacks: for Lab, CalRGB and CalGray spaces of
 * the white points and parameters the Rust tests make too, each colour's components and the
 * bits of the three floats {@code toRGB} gives.
 * <p>
 * Usage: {@code java -cp <pdfbox jars> CieColours.java <output.tsv>}
 */
public final class CieColours {

    private static COSArray numbers(float... values) {
        COSArray array = new COSArray();
        for (float value : values) {
            array.add(new COSFloat(value));
        }
        return array;
    }

    private static COSArray space(COSName kind, COSDictionary dictionary) {
        COSArray array = new COSArray();
        array.add(kind);
        array.add(dictionary);
        return array;
    }

    public static void main(String[] args) throws IOException {
        COSDictionary lab = new COSDictionary();
        lab.setItem(COSName.WHITE_POINT, numbers(0.9505f, 1f, 1.089f));
        lab.setItem(COSName.RANGE, numbers(-100f, 100f, -100f, 100f));
        COSDictionary calRgb = new COSDictionary();
        calRgb.setItem(COSName.WHITE_POINT, numbers(1f, 1f, 1f));
        calRgb.setItem(COSName.GAMMA, numbers(2.2f, 2.2f, 2.2f));
        calRgb.setItem(COSName.MATRIX, numbers(0.4124f, 0.2126f, 0.0193f, 0.3576f, 0.7152f,
                0.1192f, 0.1805f, 0.0722f, 0.9505f));
        COSDictionary calGray = new COSDictionary();
        calGray.setItem(COSName.WHITE_POINT, numbers(1f, 1f, 1f));
        calGray.setItem(COSName.GAMMA, new COSFloat(2.2f));
        PDColorSpace[] spaces = {
            new PDLab(space(COSName.LAB, lab)),
            new PDCalRGB(space(COSName.CALRGB, calRgb)),
            new PDCalGray(space(COSName.CALGRAY, calGray)),
        };
        float[][][] colours = {
            {{0, 0, 0}, {100, 0, 0}, {50, 20, -30}, {75.5f, -60, 45}, {12, 90, 90}, {33, -100, -100}},
            {{0, 0, 0}, {1, 1, 1}, {0.5f, 0.25f, 0.75f}, {0.9f, 0.1f, 0.3f}, {0.02f, 0.6f, 1}},
            {{0}, {1}, {0.5f}, {0.2f}, {0.85f}},
        };
        try (PrintWriter out = new PrintWriter(Files.newBufferedWriter(Path.of(args[0]),
                StandardCharsets.UTF_8))) {
            out.print("# colour space\tcomponents\tred green blue (float bits)\n");
            for (int s = 0; s < spaces.length; s++) {
                for (float[] colour : colours[s]) {
                    float[] rgb = spaces[s].toRGB(colour.clone());
                    StringBuilder line = new StringBuilder(spaces[s].getName()).append('\t');
                    for (int i = 0; i < colour.length; i++) {
                        line.append(i == 0 ? "" : " ").append(colour[i]);
                    }
                    line.append('\t');
                    for (int i = 0; i < 3; i++) {
                        line.append(i == 0 ? "" : " ")
                                .append(String.format("%08x", Float.floatToRawIntBits(rgb[i])));
                    }
                    out.print(line.append('\n'));
                }
            }
        }
    }
}
