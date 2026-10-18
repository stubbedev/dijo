import java.awt.color.ColorSpace;
import java.awt.color.ICC_Profile;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.StandardCopyOption;

/**
 * Writes the CIE XYZ profile PDFBox converts the colours of its CIE-based colour spaces through:
 * the data of {@code ICC_Profile.getInstance(ColorSpace.CS_CIEXYZ)}.
 */
public final class JdkCieXyzProfile {

    public static void main(String[] args) throws Exception {
        if (args.length != 1) {
            System.err.println("usage: JdkCieXyzProfile <output>");
            System.exit(2);
        }
        final Path output = Path.of(args[0]);
        final Path temporary = output.resolveSibling(output.getFileName() + ".tmp");
        Files.write(temporary, ICC_Profile.getInstance(ColorSpace.CS_CIEXYZ).getData());
        Files.move(temporary, output, StandardCopyOption.REPLACE_EXISTING,
                StandardCopyOption.ATOMIC_MOVE);
    }
}
