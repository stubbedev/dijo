import java.io.IOException;
import java.io.PrintWriter;
import java.lang.reflect.Field;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;

/**
 * Writes the tables of the JFlex scanners jhighlight highlights Java, Groovy and C++ with, as
 * Tika's SourceCodeParser renders source code through them, as the Rust source the native
 * scanner runs on: each table run-length encoded as (count, value) pairs.
 * Usage: {@code java -cp jhighlight.jar LexerTables.java <tables.rs>}
 */
public class LexerTables {

    private static final String[][] LEXERS = {
        {"JAVA", "JavaHighlighter"},
        {"GROOVY", "GroovyHighlighter"},
        {"CPP", "CppHighlighter"},
    };

    private static final String[] TABLES = {"ZZ_CMAP", "ZZ_ACTION", "ZZ_ROWMAP", "ZZ_TRANS", "ZZ_ATTRIBUTE"};

    public static void main(String[] args) throws Exception {
        StringBuilder out = new StringBuilder();
        out.append("//! The tables of the JFlex 1.4.1 scanners jhighlight 2.0.0 highlights Java, Groovy and\n");
        out.append("//! C++ with, each run-length encoded as (count, value) pairs, as `just source-code-tables`\n");
        out.append("//! writes them from the scanners' classes.\n\n");
        out.append("use super::lexer::Tables;\n");
        for (String[] lexer : LEXERS) {
            Class<?> type = Class.forName("org.codelibs.jhighlight.highlighter." + lexer[1]);
            out.append("\n/// The tables of jhighlight's `").append(lexer[1]).append("`.\n");
            out.append("pub(super) const ").append(lexer[0]).append(": Tables = Tables {\n");
            for (String table : TABLES) {
                out.append("    ").append(field(table)).append(": &")
                        .append(lexer[0]).append('_').append(table.substring(3)).append(",\n");
            }
            out.append("};\n");
            for (String table : TABLES) {
                int[] values = values(type, table);
                StringBuilder runs = new StringBuilder();
                int count = 0;
                for (int i = 0; i < values.length; ) {
                    int value = values[i];
                    int run = 0;
                    while (i < values.length && values[i] == value) {
                        run++;
                        i++;
                    }
                    runs.append('(').append(run).append(", ").append(value).append("), ");
                    count++;
                }
                out.append("\n#[rustfmt::skip]\n");
                out.append("static ").append(lexer[0]).append('_').append(table.substring(3))
                        .append(": [(u32, i32); ").append(count).append("] = [")
                        .append(runs.toString().trim()).append("];\n");
            }
        }
        Files.writeString(Path.of(args[0]), out.toString(), StandardCharsets.UTF_8);
    }

    private static String field(String table) {
        return switch (table) {
            case "ZZ_CMAP" -> "character_classes";
            case "ZZ_ACTION" -> "actions";
            case "ZZ_ROWMAP" -> "rows";
            case "ZZ_TRANS" -> "transitions";
            case "ZZ_ATTRIBUTE" -> "attributes";
            default -> throw new IllegalArgumentException(table);
        };
    }

    private static int[] values(Class<?> type, String table) throws ReflectiveOperationException {
        Field field = type.getDeclaredField(table);
        field.setAccessible(true);
        Object value = field.get(null);
        if (value instanceof char[] chars) {
            int[] values = new int[chars.length];
            for (int i = 0; i < chars.length; i++) {
                values[i] = chars[i];
            }
            return values;
        }
        return (int[]) value;
    }
}
