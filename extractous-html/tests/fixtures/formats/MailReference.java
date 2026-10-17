import org.apache.james.mime4j.codec.DecodeMonitor;
import org.apache.james.mime4j.codec.DecoderUtil;
import org.apache.james.mime4j.dom.address.Address;
import org.apache.james.mime4j.dom.address.AddressList;
import org.apache.james.mime4j.dom.address.Mailbox;
import org.apache.james.mime4j.dom.field.AddressListField;
import org.apache.james.mime4j.dom.field.ContentDispositionField;
import org.apache.james.mime4j.dom.field.ContentTypeField;
import org.apache.james.mime4j.field.DefaultFieldParser;
import org.apache.james.mime4j.field.LenientFieldParser;
import org.apache.james.mime4j.stream.EntityState;
import org.apache.james.mime4j.stream.Field;
import org.apache.james.mime4j.stream.MimeConfig;
import org.apache.james.mime4j.stream.MimeTokenStream;
import org.apache.james.mime4j.stream.RawField;
import org.apache.tika.detect.DefaultDetector;
import org.apache.tika.metadata.Message;
import org.apache.tika.metadata.Metadata;
import org.apache.tika.parser.mailcommons.MailDateParser;
import org.apache.tika.parser.mailcommons.MailUtil;
import org.apache.tika.utils.DateUtils;

import java.io.ByteArrayInputStream;
import java.nio.charset.StandardCharsets;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.Base64;
import java.util.Date;
import java.util.LinkedHashSet;
import java.util.List;
import java.util.Locale;
import java.util.Map;
import java.util.Set;
import java.util.TreeMap;

/**
 * Writes what mime4j 0.8.15 and Tika's mail commons make of email header values, which the
 * Rust port of Tika's RFC822Parser is tested against: for each value, its kind, the value in
 * base64, and the answer in base64, or {@code null}.
 *
 * <ul>
 * <li>{@code date}: {@code MailDateParser.parseDateLenient}, as {@code Metadata} writes the date;
 * <li>{@code words}: {@code DecoderUtil.decodeEncodedWords} with a silent monitor;
 * <li>{@code ctype}: the strict {@code ContentTypeField} a body descriptor reads: the MIME type,
 * media type and subtype, and the parameters in name order;
 * <li>{@code cdisp}: the strict {@code ContentDispositionField}: the type, then the parameters in
 * the order Java's map lists them;
 * <li>{@code to} and {@code from}: the lenient address list as {@code MailContentHandler} writes
 * each address, the {@code from} list flattened and each followed by what
 * {@code MailUtil.setPersonAndEmail} makes of it.
 * </ul>
 *
 * <p>The values are those of the header fields of every message of the given corpora that Tika's
 * default detector gives an email type, and synthetic ones. Fields of a value are separated by
 * U+001F, a name from its value by U+001E.
 *
 * <p>Usage: {@code MailReference <fixture> <label=dir>...}
 */
final class MailReference {

    private static final char SEPARATOR = '\u001f';
    private static final char PAIR = '\u001e';
    private static final Set<String> TYPES = Set.of("message/rfc822", "message/x-emlx");

    private MailReference() {
    }

    public static void main(String[] args) throws Exception {
        final Map<String, Set<String>> values = new TreeMap<>();
        for (String kind : List.of("date", "words", "ctype", "cdisp", "to", "from")) {
            values.put(kind, new LinkedHashSet<>());
        }
        final String[] roots = java.util.Arrays.copyOfRange(args, 1, args.length);
        for (ParserReference.Input input :
                ParserReference.detected(new DefaultDetector(), TYPES, roots)) {
            harvest(input.bytes(), values);
        }
        synthetic(values);
        mutate(values);
        final List<String> lines = new ArrayList<>();
        for (Map.Entry<String, Set<String>> entry : values.entrySet()) {
            for (String value : entry.getValue()) {
                final String answer = answer(entry.getKey(), value);
                lines.add(entry.getKey() + '\t' + base64(value) + '\t'
                        + (answer == null ? "null" : base64(answer)));
            }
        }
        ParserReference.write(Path.of(args[0]), lines);
    }

    private static String base64(String text) {
        return Base64.getEncoder().encodeToString(text.getBytes(StandardCharsets.UTF_8));
    }

    /**
     * Collects the bodies of the fields of every part of a message by the kind they are read
     * as, up to where mime4j stops reading the message if it does.
     */
    private static void harvest(byte[] message, Map<String, Set<String>> values) throws Exception {
        final MimeConfig config = new MimeConfig.Builder().setMaxLineLen(100000)
                .setMaxHeaderLen(100000).build();
        final MimeTokenStream stream = new MimeTokenStream(config);
        stream.parse(new ByteArrayInputStream(message));
        try {
            for (EntityState state = stream.getState(); state != EntityState.T_END_OF_STREAM;
                 state = stream.next()) {
                if (state != EntityState.T_FIELD) {
                    continue;
                }
                final Field field = stream.getField();
                final String body = field.getBody();
                values.get("words").add(body);
                switch (field.getName().toLowerCase(Locale.ROOT)) {
                    case "date", "resent-date" -> values.get("date").add(body);
                    case "content-type" -> values.get("ctype").add(body);
                    case "content-disposition" -> values.get("cdisp").add(body);
                    case "to", "cc", "bcc", "reply-to", "resent-to" -> values.get("to").add(body);
                    case "from", "sender", "resent-from" -> {
                        values.get("from").add(body);
                        values.get("to").add(body);
                    }
                    default -> {
                    }
                }
            }
        } catch (org.apache.james.mime4j.MimeException | java.io.IOException e) {
            return;
        }
    }

    private static String answer(String kind, String value) throws Exception {
        return switch (kind) {
            case "date" -> {
                final Date date = MailDateParser.parseDateLenient(value);
                yield date == null ? null : DateUtils.formatDate(date);
            }
            case "words" -> DecoderUtil.decodeEncodedWords(value, DecodeMonitor.SILENT);
            case "ctype" -> contentType(value);
            case "cdisp" -> contentDisposition(value);
            case "to" -> addresses(value, false);
            case "from" -> addresses(value, true);
            default -> throw new IllegalArgumentException(kind);
        };
    }

    private static String contentType(String value) {
        final ContentTypeField field = (ContentTypeField) DefaultFieldParser.getParser()
                .parse(new RawField("Content-Type", value), DecodeMonitor.SILENT);
        final StringBuilder out = new StringBuilder();
        out.append(field.getMimeType()).append(SEPARATOR).append(field.getMediaType())
                .append(SEPARATOR).append(field.getSubType());
        for (Map.Entry<String, String> parameter : new TreeMap<>(field.getParameters()).entrySet()) {
            out.append(SEPARATOR).append(parameter.getKey()).append(PAIR)
                    .append(parameter.getValue());
        }
        return out.toString();
    }

    private static String contentDisposition(String value) {
        final ContentDispositionField field = (ContentDispositionField) DefaultFieldParser
                .getParser().parse(new RawField("Content-Disposition", value), DecodeMonitor.SILENT);
        final StringBuilder out = new StringBuilder(field.getDispositionType());
        for (Map.Entry<String, String> parameter : field.getParameters().entrySet()) {
            out.append(SEPARATOR).append(parameter.getKey()).append(PAIR)
                    .append(parameter.getValue());
        }
        return out.toString();
    }

    /** The addresses as {@code MailContentHandler} reads a {@code To} or a {@code From} field. */
    private static String addresses(String value, boolean from) {
        final AddressListField field = (AddressListField) LenientFieldParser.getParser()
                .parse(new RawField("To", value), DecodeMonitor.SILENT);
        final AddressList list = field.getAddressList();
        final List<String> out = new ArrayList<>();
        final List<? extends Address> addresses = from ? list.flatten() : list;
        for (Address address : addresses) {
            final String display = display(address);
            out.add(display);
            if (from) {
                final Metadata metadata = new Metadata();
                MailUtil.setPersonAndEmail(display, Message.MESSAGE_FROM_NAME,
                        Message.MESSAGE_FROM_EMAIL, metadata);
                out.add(metadata.get(Message.MESSAGE_FROM_NAME) + PAIR
                        + metadata.get(Message.MESSAGE_FROM_EMAIL));
            }
        }
        return String.join(String.valueOf(SEPARATOR), out);
    }

    /** {@code MailContentHandler.getDisplayString}. */
    private static String display(Address address) {
        if (address instanceof Mailbox mailbox) {
            String name = mailbox.getName();
            if (name != null && name.length() > 0) {
                name = DecoderUtil.decodeEncodedWords(name, DecodeMonitor.SILENT);
                return name + " <" + mailbox.getAddress() + ">";
            }
            return mailbox.getAddress();
        }
        return address.toString();
    }

    /** Characters the mutations insert: the specials of the grammars, white space and more. */
    private static final String INSERTED = "()<>@,;:\\\"/[]?=_.+-*'%~ \t09aZ\u00e9\u0151\u20ac";

    /**
     * Adds to each kind's values five mutants of each, made with a fixed seed: a character left
     * out, a character put in, a run repeated, the case changed, and the value cut short.
     */
    private static void mutate(Map<String, Set<String>> values) {
        final java.util.Random random = new java.util.Random(41);
        for (Set<String> kind : values.values()) {
            for (String value : new ArrayList<>(kind)) {
                if (value.isEmpty()) {
                    continue;
                }
                final int at = random.nextInt(value.length());
                final int to = at + random.nextInt(value.length() - at) + 1;
                kind.add(value.substring(0, at) + value.substring(at + 1));
                kind.add(value.substring(0, at) + INSERTED.charAt(random.nextInt(INSERTED.length()))
                        + value.substring(at));
                kind.add(value.substring(0, to) + value.substring(at));
                kind.add(random.nextBoolean() ? value.toUpperCase(Locale.ROOT)
                        : value.toLowerCase(Locale.ROOT));
                kind.add(value.substring(0, at));
            }
        }
    }

    private static void synthetic(Map<String, Set<String>> values) {
        values.get("date").addAll(List.of(
                "Tue, 05 Mar 2024 14:30:00 +0100",
                "Wed, 26 Jan 2022 09:14:37 +0100 (CET)",
                "Mon, 9 May 2016 3:32:00 +0200",
                "Mon, 9 May 2016 7:32:00 UTC+0600 (BST)",
                "Mon, 9 May 2016 7:32:00 UTC+6",
                "Mon, 9 May 2016 7:32:00 GMT-0330",
                "16 May 2016 at 09:30:32  GMT+1",
                "9 May 2016 01:32:00 UTC",
                "9 May 2016 01:32:00 GMT",
                "9 May 2016 01:32:00 UT",
                "9 May 2016 01:32:00 Z",
                "9 May 2016 01:32:00 z",
                "9 May 2016 01:32:00 GMT0",
                "9 May 2016 01:32:00 UTC0",
                "9 May 2016 01:32:00",
                "9 May 2016 01:32",
                "9 May 2016 1:2",
                "9 May 16 01:32:00 +0000",
                "9 May 69 01:32:00 +0000",
                "9 May 70 01:32:00 +0000",
                "9 May 916 01:32:00 +0000",
                "9 May 1 01:32:00 +0000",
                "09 MAY 2016 01:32:00.123 +0000",
                "09 MAY 2016 01:32:00.12345 +0000",
                "9 May 2016 25:32:00 +0000",
                "31 Feb 2021 10:00",
                "0 Jan 2021 10:00",
                "9 September 2016 01:32:00 +0000",
                "9 Sept 2016 01:32:00 +0000",
                "9 J 2016 01:32:00 +0000",
                "9 5 2016 01:32:00 +0000",
                "9 13 2016 01:32:00 +0000",
                "July 9 2012 10:10:10 am UTC",
                "July 9 2012 10:10:10am",
                "July 9 2012 12:10:10 PM",
                "July 9 2012 12:10:10 AM",
                "July 9 2012 10:10 p",
                "Jul 9 2012 10:10:10",
                "9 Jul 2012 10:10:10 PM +0200",
                "2012-10-10 10:10:10 UTC",
                "2012-10-10 10:10:10",
                "2012-10-10 10:10",
                "2012-10-10",
                "2012-1-10",
                "7/9/2012 10:10:10",
                "7/9/2012 10:10:10 PM",
                "7/9/12 10 AM",
                "7/9/2012",
                "13/9/2012",
                "31/12/99",
                "12/31/99",
                "99/12/31",
                "2012/12/31",
                "Jul 9 2012",
                "9 Jul 2012",
                "9 Jul 12",
                "Jul 9, 2012",
                "Thursday, July 9 2012 10:10 +0100",
                "Thu, 9 Jul 2012 10:10:10 EST",
                "Thu, 9 Jul 2012 10:10:10 EDT",
                "Thu, 9 Jul 2012 10:10:10 PST",
                "Thu, 9 Jul 2012 10:10:10 CET",
                "Thu, 9 Jul 2012 10:10:10 Europe/Berlin",
                "Thu, 9 Jul 2012 10:10:10 -0500 (EST)",
                "Thu, 9 Jul 2012 10:10:10 +2500",
                "Thu, 9 Jul 2012 10:10:10 +23:00",
                "Thu, 9 Jul 2012 10:10:10 +01:30:15",
                "Thu, 9 Jul 2012 10:10:10 +1",
                "Thu, 9 Jul 2012 10:10:10 GMT+01:00",
                "Thu, 9 Jul 2012 10:10:10 UT+0100",
                "10-10-2000 10:10 -0500",
                "Sun, 31 Dec 1899 23:59:59 +0000",
                "Fri, 31 Dec 9999 23:59:59 +0000",
                "1 Jan 999999999 00:00 +0000",
                "+5 Jan 2020 10:00",
                "-5 Jan 2020 10:00",
                "5 Jan +2020 10:00",
                "5 Jan 2020 +10:00",
                "Mon, 5 Jan 2020 10:00 +0100\n",
                "",
                "   ",
                "not a date",
                "Thu Jul 9 2012"));
        values.get("words").addAll(List.of(
                "=?utf-8?q?caf=C3=A9?=",
                "=?utf-8?b?w6k=?= =?utf-8?q?=C3=A9?=",
                "=?utf-8?q?=C3?= =?utf-8?q?=A9?=",
                "=?UTF-8?Q?a_b?=",
                "a =?iso-8859-1?q?=E9?= b",
                "=?iso-8859-1?q?=E9?= x =?iso-8859-1?q?=E9?=",
                "=?x-unknown?q?a?= z",
                "=?utf-8?x?a?=",
                "=?utf-8?q??=",
                "=?utf-8*en?q?a?=",
                "broken =?utf-8 x",
                "=?utf-8?q?a?b?=",
                "=?windows-1252?q?=80?=",
                "=?gb2312?b?1tDOxA==?=",
                "=?shift_jis?b?k/qWe4zq?=",
                "=?iso-2022-jp?b?GyRCRnxLXDhsGyhC?=",
                "=?utf-8?b?w6k?=",
                "=?utf-8?b?w6k=extra?=",
                "=?utf-8?q?trailing=?=",
                "=?utf-8?q?=3D=3d=zz?=",
                "pre=?utf-8?q?x?=post",
                "=?utf-8?q?é?="));
        values.get("ctype").addAll(List.of(
                "text/plain",
                "Text/Plain; Charset=\"utf-8\" (comment); name=a b",
                "text/plain; x=123; y=12a",
                "text",
                "123/x",
                "multipart/mixed; boundary=\"abc",
                "text/plain; a=1; name=\"Ā\"; b=2",
                "multipart/alternative; boundary=\"--==_mimepart\"",
                "multipart/mixed;\tboundary=b ; charset=x",
                "text/plain;",
                "text/plain; ;",
                "text/plain; =x",
                "text/html; charset=\"windows-1252\\\"\"",
                "application/octet-stream; name=\"a\\\\b.pdf\"",
                "text/plain; NAME=A; name=b",
                "message/rfc822",
                "text/plain (comment (nested)) ; charset=us-ascii",
                "text/plain; charset=us-ascii (unclosed"));
        values.get("cdisp").addAll(List.of(
                "attachment",
                "Attachment; filename*0*=utf-8''caf%C3%A9; filename*1=.txt; size=12",
                "inline; filename=a/b.txt",
                "attachment; filename=\"=?utf-8?q?r=C3=A9sum=C3=A9?=\"",
                "attachment; filename*=UTF-8''r%C3%A9sum%C3%A9.pdf",
                "attachment; filename*=iso-8859-1'en'%E9t%E9.txt",
                "attachment; filename*=x-unknown''a%20b",
                "attachment; filename*=utf-8''%ZZ",
                "attachment; filename=\"a b.txt\"; creation-date=\"Tue, 05 Mar 2024 14:30:00 +0100\"",
                "attachment; filename=a+b.txt; name*=utf-8''x",
                "attachment; filename=\"Szőlő.pdf\"",
                "attachment;",
                ";",
                "attachment; filename",
                "attachment; a=1; b=2; c=3; d=4; e=5; f=6; g=7; h=8"));
        values.get("to").addAll(List.of(
                "Ada Lovelace <ada@example.org>, bob@example.org",
                "\"Babbage, Charles\" (inventor) <charles@example.org>",
                "=?utf-8?q?Ren=C3=A9?= <rene@example.org>",
                "Team: a@x.org, B <b@y.org>; c@z.org",
                "<@route,@r2:user@host>",
                "undisclosed-recipients:;",
                "just text",
                "a@b, , c@d",
                "<a@b",
                "Name <a@b> trailing",
                "\"=?utf-8?q?Ren=C3=A9?=\" <r@e>",
                "\"=?utf-8?q?=3D=3Futf-8=3Fq=3Fx=3F=3D?=\" <r@e>",
                "René <r@e>",
                "a@b@c",
                "@x",
                "<>"));
        values.get("from").addAll(List.of(
                "Ada Lovelace <ada@example.org>",
                "ada@example.org",
                "\"Name\"",
                "G: a@x, b@y;, c@z",
                "\"a@b\" <c@d>",
                "x <a@b> <c@d>",
                "Novell GroupWise <$$$.$$$.$$$>"));
    }
}
