import java.io.FileOutputStream;
import java.io.OutputStreamWriter;
import java.io.Writer;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.Calendar;
import java.util.List;
import java.util.TimeZone;
import java.util.zip.GZIPOutputStream;

import org.apache.pdfbox.util.DateConverter;

/**
 * Writes how PDFBox's DateConverter reads dates that end in a time zone name: every zone ID the
 * JDK knows and names it does not, after dates across the years the JDK keeps rules for. Each line
 * is the text, then the instant and the offset of the calendar's zone at it, or "null".
 */
public class NamedZoneDates {
    private static final String[] DATES = {
        "1905-03-01T12:00:00",
        "1950-01-15T08:30:00",
        "1985-07-01T02:30:00",
        "2010-01-28T11:02:12",
        "2010-07-28T11:02:12.000",
        "2030-10-27 02:30:00",
    };

    private static final String[] UNKNOWN = {
        "CEST", "cet", "europe/copenhagen", "XYZ", "ROC", "Factory", "posixrules", "Mars/Olympus",
    };

    public static void main(String[] args) throws Exception {
        List<String> names = new ArrayList<>(List.of(TimeZone.getAvailableIDs()));
        names.addAll(List.of(UNKNOWN));
        try (Writer out = new OutputStreamWriter(
                new GZIPOutputStream(new FileOutputStream(args[0])), StandardCharsets.UTF_8)) {
            for (String name : names) {
                for (String date : DATES) {
                    String text = date + (name.length() % 2 == 0 ? " " : "") + name;
                    Calendar calendar = DateConverter.toCalendar(text);
                    out.write(text);
                    out.write('\t');
                    if (calendar == null) {
                        out.write("null");
                    } else {
                        out.write(calendar.toInstant().toString());
                        out.write('\t');
                        out.write(Integer.toString(calendar.get(Calendar.ZONE_OFFSET)
                                + calendar.get(Calendar.DST_OFFSET)));
                    }
                    out.write('\n');
                }
            }
        }
    }
}
