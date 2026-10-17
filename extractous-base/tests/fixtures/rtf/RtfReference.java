import java.io.FileOutputStream;
import java.io.IOException;
import java.io.OutputStreamWriter;
import java.io.PrintWriter;
import java.nio.charset.Charset;
import java.nio.charset.StandardCharsets;
import java.util.Calendar;
import java.util.Date;
import java.util.Locale;
import java.util.TimeZone;
import java.util.zip.GZIPOutputStream;

import org.apache.tika.utils.CharsetUtils;
import org.apache.tika.utils.DateUtils;

/**
 * Writes what Tika's RTFParser makes of the code pages it names and of the dates in an RTF
 * document's info group, for the Rust port to be tested against:
 * <ul>
 * <li>{@code charsets.tsv}: each charset name TextExtractor asks CharsetUtils for, and the
 * charset it gets, US-ASCII where CharsetUtils has none;</li>
 * <li>{@code dates.tsv.gz}: the zone, year, month, day, hour and minute a lenient
 * GregorianCalendar is set to, and the date DateUtils formats from it; and the epoch
 * milliseconds and formatted date of instants.</li>
 * </ul>
 * Usage: {@code java -cp <tika-core and its dependencies> RtfReference.java <directory>}
 */
public class RtfReference {

    private static final String[] CHARSETS = {
        "WINDOWS-1252", "MacRoman", "Shift_JIS", "windows-57011", "windows-57010",
        "windows-57009", "windows-57008", "windows-57007", "windows-57006", "windows-57005",
        "windows-57004", "windows-57003", "x-ISCII91", "x-MacCentralEurope", "MacCyrillic",
        "x-Johab", "CP1258", "CP1257", "CP1256", "CP1255", "CP1254", "CP1253", "CP1252",
        "CP1251", "CP1250", "CP950", "CP949", "MS936", "MS874", "CP866", "CP865", "CP864",
        "CP863", "CP862", "CP860", "CP852", "CP850", "CP819", "windows-720", "windows-711",
        "windows-710", "windows-709", "ISO-8859-6", "CP437", "cp850", "cp437", "ms874",
        "cp1257", "cp1256", "cp1255", "cp1258", "cp1254", "cp1253", "ms950", "ms936", "ms1361",
        "MS932", "cp1251", "cp1250", "MacThai", "MacTurkish", "MacGreek", "MacArabic",
        "MacHebrew", "johab", "Big5", "GB2312", "ms949",
    };

    private static final String[] WIDE_ZONES = {"UTC", "Europe/Copenhagen"};
    private static final int[] WIDE_YEARS = {
        Integer.MIN_VALUE, -1000000, -10000, -1, 0, 1, 4, 100, 1000, 1581, 1582, 1583, 1899,
        1900, 1970, 2000, 2038, 2100, 10000, 1000000, Integer.MAX_VALUE,
    };
    private static final int[] WIDE_MONTHS = {
        Integer.MIN_VALUE, -25, -1, 0, 1, 9, 11, 12, Integer.MAX_VALUE,
    };
    private static final int[] WIDE_DAYS = {
        Integer.MIN_VALUE, -400, 0, 1, 5, 14, 29, 31, Integer.MAX_VALUE,
    };
    private static final int[][] WIDE_TIMES = {{0, 0}, {23, 59}, {25, -61}};

    private static final String[] CHANGE_ZONES = {
        "Europe/Copenhagen", "America/New_York", "Australia/Sydney", "Asia/Kolkata",
        "Europe/Dublin", "UTC",
    };
    private static final int[] CHANGE_YEARS = {
        1916, 1945, 1980, 1996, 2006, 2024, 2037, 2038, 2060, 2400,
    };
    private static final int[][] CHANGE_DAYS = {
        {2, 8}, {2, 9}, {2, 10}, {2, 11}, {2, 12}, {2, 13}, {2, 14},
        {2, 25}, {2, 26}, {2, 27}, {2, 28}, {2, 29}, {2, 30}, {2, 31},
        {3, 1}, {3, 2}, {3, 3}, {3, 4}, {3, 5}, {3, 6}, {3, 7},
        {9, 1}, {9, 2}, {9, 3}, {9, 4}, {9, 5}, {9, 6}, {9, 7},
        {9, 25}, {9, 26}, {9, 27}, {9, 28}, {9, 29}, {9, 30}, {9, 31},
        {10, 1}, {10, 2}, {10, 3}, {10, 4}, {10, 5}, {10, 6}, {10, 7},
    };
    private static final int[] CHANGE_HOURS = {0, 1, 2, 3};

    private static final long[] INSTANTS = {
        Long.MIN_VALUE, Long.MIN_VALUE + 999, -377705116800001L, -377705116800000L,
        -62198758800000L, -62167219200001L, -62167219200000L, -12219292800000L, -1L, 0L, 999L,
        1000L, 253402300799999L, 253402300800000L, 315537897600000L, Long.MAX_VALUE,
    };

    public static void main(String[] args) throws IOException {
        String directory = args[0];
        try (PrintWriter out = new PrintWriter(new OutputStreamWriter(
                new FileOutputStream(directory + "/charsets.tsv"), StandardCharsets.UTF_8))) {
            for (String name : CHARSETS) {
                Charset charset;
                try {
                    charset = CharsetUtils.forName(name);
                } catch (IllegalArgumentException e) {
                    charset = Charset.forName("US-ASCII");
                }
                out.print(name + "\t" + charset.name() + "\n");
            }
        }
        try (PrintWriter out = new PrintWriter(new OutputStreamWriter(new GZIPOutputStream(
                new FileOutputStream(directory + "/dates.tsv.gz")), StandardCharsets.UTF_8))) {
            for (String zone : WIDE_ZONES) {
                for (int year : WIDE_YEARS) {
                    for (int month : WIDE_MONTHS) {
                        for (int day : WIDE_DAYS) {
                            for (int[] time : WIDE_TIMES) {
                                date(out, zone, year, month, day, time[0], time[1]);
                            }
                        }
                    }
                }
            }
            for (String zone : CHANGE_ZONES) {
                for (int year : CHANGE_YEARS) {
                    for (int[] monthDay : CHANGE_DAYS) {
                        for (int hour : CHANGE_HOURS) {
                            date(out, zone, year, monthDay[0], monthDay[1], hour, 30);
                        }
                    }
                }
            }
            for (long millis : INSTANTS) {
                out.print("instant\t" + millis + "\t" + DateUtils.formatDate(new Date(millis)) + "\n");
            }
        }
    }

    private static void date(PrintWriter out, String zone, int year, int month, int day, int hour,
                             int minute) {
        Calendar calendar = Calendar.getInstance(TimeZone.getTimeZone(zone), Locale.ROOT);
        calendar.set(year, month, day, hour, minute, 0);
        calendar.set(Calendar.MILLISECOND, 0);
        out.print(zone + "\t" + year + "\t" + month + "\t" + day + "\t" + hour + "\t" + minute
                + "\t" + DateUtils.formatDate(calendar.getTime()) + "\n");
    }
}
