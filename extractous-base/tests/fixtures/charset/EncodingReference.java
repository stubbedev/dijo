import org.apache.tika.detect.BOMDetector;
import org.apache.tika.detect.CompositeEncodingDetector;
import org.apache.tika.detect.EncodingResult;
import org.apache.tika.detect.MetadataCharsetDetector;
import org.apache.tika.detect.html.HtmlEncodingDetector;
import org.apache.tika.io.TikaInputStream;
import org.apache.tika.metadata.HttpHeaders;
import org.apache.tika.metadata.Metadata;
import org.apache.tika.metadata.TikaCoreProperties;
import org.apache.tika.ml.chardetect.MojibusterEncodingDetector;
import org.apache.tika.ml.junkdetect.JunkDetector;
import org.apache.tika.ml.junkdetect.JunkFilterEncodingDetector;
import org.apache.tika.parser.ParseContext;

import java.io.ByteArrayOutputStream;
import java.io.IOException;
import java.io.OutputStream;
import java.nio.charset.Charset;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.Base64;
import java.util.List;
import java.util.Locale;
import java.util.Random;
import java.util.stream.Collectors;
import java.util.stream.Stream;
import java.util.zip.GZIPOutputStream;

/**
 * Writes what Apache Tika's default encoding detection answers for every input the Rust port is
 * checked against: paragraphs of the quality truth texts written in each charset that can hold
 * them, as plain text and as HTML declaring their charset rightly, wrongly or not at all; the
 * legacy-encoding corpus; the text, HTML and XML documents of Tika's own test corpus, cut to
 * 64 KiB; and edge cases. A line, gzipped, is the base64 bytes, the content type the detection
 * is told, then what AutoDetectReader decodes with, the charset detected, the detector that
 * chose it, and Mojibuster's own candidates.
 *
 * Tika's DefaultEncodingDetector runs its detectors in the order their jars come on the class
 * path, which it does not sort, so the composite is built here in the order the extractous native
 * image's class path gives them: tika-encoding-detector-html, tika-ml-junkdetect (the
 * arbitrating JunkFilter), tika-encoding-detector-mojibuster, then tika-core's BOMDetector and
 * MetadataCharsetDetector. The class path starts with the encoding rules NativeImageRules.java
 * compiles, which the native image substitutes for Tika's.
 */
public final class EncodingReference {

    private static final List<String> CHARSETS = List.of("UTF-8", "UTF-16LE", "UTF-16BE",
            "UTF-32LE", "UTF-32BE", "windows-1250", "windows-1251", "windows-1252",
            "windows-1253", "windows-1254", "windows-1255", "windows-1256", "windows-1257",
            "windows-1258", "ISO-8859-1", "ISO-8859-2", "ISO-8859-3", "ISO-8859-4", "ISO-8859-5",
            "ISO-8859-7", "ISO-8859-9", "ISO-8859-13", "ISO-8859-15", "ISO-8859-16", "KOI8-R",
            "KOI8-U", "IBM437", "IBM850", "IBM852", "IBM855", "IBM866", "x-MacRoman",
            "x-MacCyrillic", "x-windows-874", "Shift_JIS", "EUC-JP", "GB18030", "Big5", "EUC-KR",
            "ISO-2022-JP", "IBM500", "IBM1047");
    private static final int PARAGRAPHS_PER_TEXT = 2;
    private static final int MAX_DOCUMENT_BYTES = 64 * 1024;
    private static final List<String> DOCUMENT_EXTENSIONS = List.of(".txt", ".html", ".htm",
            ".xhtml", ".xml", ".csv", ".tsv");

    private record Input(byte[] bytes, String contentType) {
    }

    public static void main(String[] args) throws IOException {
        if (args.length != 4) {
            System.err.println(
                    "usage: EncodingReference <truth dir> <corpus dir> <tika test documents> <output>");
            System.exit(2);
        }
        final List<Input> inputs = new ArrayList<>();
        try (Stream<Path> truths = Files.list(Path.of(args[0]))) {
            for (Path truth : truths.sorted().toList()) {
                final String text = Files.readString(truth, StandardCharsets.UTF_8);
                final List<String> paragraphs = Arrays.stream(text.split("\n\\s*\n"))
                        .map(String::strip).filter(p -> p.length() > 40).toList();
                for (int i = 0; i < paragraphs.size() && i < PARAGRAPHS_PER_TEXT * 3;
                     i += 3) {
                    addEncodings(inputs, paragraphs.get(i));
                }
            }
        }
        try (Stream<Path> files = Files.list(Path.of(args[1]))) {
            for (Path file : files.sorted().toList()) {
                if (file.getFileName().toString().endsWith(".txt")) {
                    inputs.add(new Input(Files.readAllBytes(file), "text/plain"));
                }
            }
        }
        try (Stream<Path> files = Files.walk(Path.of(args[2]))) {
            for (Path file : files.filter(Files::isRegularFile).sorted().toList()) {
                final String name = file.getFileName().toString().toLowerCase(Locale.ROOT);
                final String extension = name.contains(".")
                        ? name.substring(name.lastIndexOf('.')) : "";
                if (!DOCUMENT_EXTENSIONS.contains(extension)) {
                    continue;
                }
                final byte[] bytes = Files.readAllBytes(file);
                inputs.add(new Input(Arrays.copyOf(bytes, Math.min(bytes.length,
                        MAX_DOCUMENT_BYTES)), contentTypeOf(extension)));
            }
        }
        addEdgeCases(inputs);

        final MojibusterEncodingDetector mojibuster = new MojibusterEncodingDetector();
        final CompositeEncodingDetector detector = new CompositeEncodingDetector(List.of(
                new HtmlEncodingDetector(), new JunkFilterEncodingDetector(
                        JunkDetector.loadFromClasspath()),
                new MojibusterEncodingDetector(), new BOMDetector(),
                new MetadataCharsetDetector()));
        final StringBuilder out = new StringBuilder();
        for (Input input : inputs) {
            final Metadata metadata = new Metadata();
            metadata.set(HttpHeaders.CONTENT_TYPE, input.contentType());
            final List<EncodingResult> results;
            try (TikaInputStream stream = TikaInputStream.get(input.bytes())) {
                results = detector.detect(stream, metadata, new ParseContext());
            }
            final String decodeAs;
            final String charset;
            if (results.isEmpty()) {
                decodeAs = "ISO-8859-1";
                charset = "ISO-8859-1";
            } else {
                decodeAs = results.get(0).getDecodeAs().name();
                charset = results.get(0).getCharset().name();
            }
            final Metadata mojibusterMetadata = new Metadata();
            mojibusterMetadata.set(HttpHeaders.CONTENT_TYPE, input.contentType());
            final String candidates = mojibuster.detect(input.bytes(), mojibusterMetadata).stream()
                    .map(r -> r.getCharset().name() + "@" + r.getConfidence() + "@"
                            + r.getResultType())
                    .collect(Collectors.joining(" "));
            out.append(Base64.getEncoder().encodeToString(input.bytes())).append('\t')
                    .append(input.contentType()).append('\t')
                    .append(decodeAs).append('\t')
                    .append(charset).append('\t')
                    .append(metadata.get(TikaCoreProperties.ENCODING_DETECTOR)).append('\t')
                    .append(candidates).append('\n');
        }
        try (OutputStream file = Files.newOutputStream(Path.of(args[3]));
             GZIPOutputStream gzip = new GZIPOutputStream(file)) {
            gzip.write(out.toString().getBytes(StandardCharsets.UTF_8));
        }
    }

    private static void addEncodings(List<Input> inputs, String paragraph) {
        for (String name : CHARSETS) {
            final Charset charset = Charset.forName(name);
            if (!charset.newEncoder().canEncode(paragraph)) {
                continue;
            }
            final byte[] bytes = paragraph.getBytes(charset);
            inputs.add(new Input(bytes, "text/plain"));
            if (name.startsWith("UTF-16") || name.startsWith("UTF-32") || name.startsWith("IBM10")
                    || name.equals("IBM500")) {
                continue;
            }
            inputs.add(new Input(html(paragraph, name, charset), "text/html"));
            inputs.add(new Input(html(paragraph, null, charset), "text/html"));
            final String wrong = name.equals("UTF-8") ? "windows-1252" : "utf-8";
            inputs.add(new Input(html(paragraph, wrong, charset), "text/html"));
        }
        for (String bom : List.of("UTF-8", "UTF-16LE", "UTF-16BE")) {
            inputs.add(new Input(("﻿" + paragraph).getBytes(Charset.forName(bom)),
                    "text/plain"));
        }
    }

    private static byte[] html(String paragraph, String declared, Charset charset) {
        final String meta = declared == null ? ""
                : "<meta http-equiv=\"Content-Type\" content=\"text/html; charset=" + declared
                + "\">";
        return ("<!DOCTYPE html>\n<html><head>" + meta + "<title>Page</title></head>\n<body><p>"
                + paragraph + "</p></body></html>\n").getBytes(charset);
    }

    private static String contentTypeOf(String extension) {
        return switch (extension) {
            case ".html", ".htm" -> "text/html";
            case ".xhtml" -> "application/xhtml+xml";
            case ".xml" -> "application/xml";
            case ".csv" -> "text/csv";
            case ".tsv" -> "text/tab-separated-values";
            default -> "text/plain";
        };
    }

    private static void addEdgeCases(List<Input> inputs) {
        inputs.add(new Input(new byte[0], "text/plain"));
        inputs.add(new Input(new byte[]{'a'}, "text/plain"));
        inputs.add(new Input("plain ASCII text only".getBytes(StandardCharsets.US_ASCII),
                "text/plain"));
        inputs.add(new Input(new byte[]{(byte) 0xEF, (byte) 0xBB, (byte) 0xBF}, "text/plain"));
        inputs.add(new Input(new byte[]{0, 0, 0, 0, 0, 0, 0, 0, 0, 0}, "text/plain"));
        inputs.add(new Input("\u001B$B$3$s$K$A$O\u001B(B world, \u001B$B@$3&\u001B(B"
                .getBytes(StandardCharsets.US_ASCII), "text/plain"));
        inputs.add(new Input("<meta charset=\"iso-8859-15\"><p>Euro € and café</p>"
                .getBytes(Charset.forName("ISO-8859-15")), "text/html"));
        inputs.add(new Input("<meta charset=\"x-user-defined\"><p>café</p>"
                .getBytes(StandardCharsets.ISO_8859_1), "text/html"));
        inputs.add(new Input("<!-- <meta charset=\"koi8-r\"> --><meta charset=\"utf-8\"><p>é</p>"
                .getBytes(StandardCharsets.UTF_8), "text/html"));
        final Random random = new Random(20261014L);
        for (int length : new int[]{2, 16, 300, 5000}) {
            final byte[] noise = new byte[length];
            random.nextBytes(noise);
            inputs.add(new Input(noise, "text/plain"));
        }
        final ByteArrayOutputStream mixed = new ByteArrayOutputStream();
        mixed.writeBytes("Café und Straße ".getBytes(StandardCharsets.UTF_8));
        mixed.writeBytes("naïve résumé".getBytes(StandardCharsets.ISO_8859_1));
        inputs.add(new Input(mixed.toByteArray(), "text/plain"));
    }
}
