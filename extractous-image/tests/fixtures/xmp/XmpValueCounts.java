import com.drew.metadata.Metadata;
import com.drew.metadata.xmp.XmpDirectory;
import com.drew.metadata.xmp.XmpReader;
import java.io.FileOutputStream;
import java.io.OutputStreamWriter;
import java.io.Writer;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.HexFormat;
import java.util.LinkedHashSet;
import java.util.List;
import java.util.Set;
import java.util.stream.Stream;
import java.util.zip.GZIPOutputStream;

/**
 * Writes what metadata-extractor's XmpReader, with XMPCore, makes of XMP packets: the packets of
 * the handwritten cases file (separated by lines of "----"), then those found in the documents
 * under the directories named. Each line is the XMP Value Count, "fails" where XMPCore throws an XMPException,
 * or "throws" where another exception escapes, then the packet in hex.
 */
public class XmpValueCounts {
    private static final byte[] BEGIN = "<?xpacket begin".getBytes(StandardCharsets.US_ASCII);
    private static final byte[] END = "<?xpacket end".getBytes(StandardCharsets.US_ASCII);
    private static final byte[] META = "<x:xmpmeta".getBytes(StandardCharsets.US_ASCII);
    private static final byte[] META_END = "</x:xmpmeta>".getBytes(StandardCharsets.US_ASCII);

    public static void main(String[] args) throws Exception {
        Set<String> packets = new LinkedHashSet<>();
        String cases = Files.readString(Path.of(args[1]), StandardCharsets.UTF_8);
        for (String packet : cases.split("\n----\n")) {
            packets.add(HexFormat.of().formatHex(packet.strip().getBytes(StandardCharsets.UTF_8)));
        }
        List<Path> documents = new ArrayList<>();
        for (int i = 2; i < args.length; i++) {
            try (Stream<Path> walk = Files.walk(Path.of(args[i]))) {
                walk.filter(Files::isRegularFile).sorted().forEach(documents::add);
            }
        }
        for (Path document : documents) {
            for (byte[] packet : find(Files.readAllBytes(document))) {
                packets.add(HexFormat.of().formatHex(packet));
            }
        }
        try (Writer out = new OutputStreamWriter(
                new GZIPOutputStream(new FileOutputStream(args[0])), StandardCharsets.UTF_8)) {
            for (String hex : packets) {
                out.write(count(HexFormat.of().parseHex(hex)));
                out.write('\t');
                out.write(hex);
                out.write('\n');
            }
        }
    }

    private static String count(byte[] packet) {
        try {
            Metadata metadata = new Metadata();
            new XmpReader().extract(packet, metadata);
            XmpDirectory directory = metadata.getFirstDirectoryOfType(XmpDirectory.class);
            if (directory == null) {
                return "empty";
            }
            if (directory.containsTag(XmpDirectory.TAG_XMP_VALUE_COUNT)) {
                return Integer.toString(directory.getInt(XmpDirectory.TAG_XMP_VALUE_COUNT));
            }
            return "fails";
        } catch (Exception e) {
            return "throws";
        }
    }

    private static List<byte[]> find(byte[] bytes) {
        List<byte[]> found = new ArrayList<>();
        int at = 0;
        while ((at = indexOf(bytes, BEGIN, at)) >= 0) {
            int end = indexOf(bytes, END, at);
            if (end < 0) {
                break;
            }
            int close = indexOf(bytes, "?>".getBytes(StandardCharsets.US_ASCII), end);
            if (close < 0 || close - at > 4 * 1024 * 1024) {
                break;
            }
            byte[] packet = new byte[close + 2 - at];
            System.arraycopy(bytes, at, packet, 0, packet.length);
            found.add(packet);
            at = close + 2;
        }
        if (found.isEmpty()) {
            at = 0;
            while ((at = indexOf(bytes, META, at)) >= 0) {
                int end = indexOf(bytes, META_END, at);
                if (end < 0 || end - at > 4 * 1024 * 1024) {
                    break;
                }
                byte[] packet = new byte[end + META_END.length - at];
                System.arraycopy(bytes, at, packet, 0, packet.length);
                found.add(packet);
                at = end + META_END.length;
            }
        }
        return found;
    }

    private static int indexOf(byte[] bytes, byte[] needle, int from) {
        outer:
        for (int i = from; i <= bytes.length - needle.length; i++) {
            for (int j = 0; j < needle.length; j++) {
                if (bytes[i + j] != needle[j]) {
                    continue outer;
                }
            }
            return i;
        }
        return -1;
    }
}
