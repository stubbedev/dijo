package ai.yobix;

import java.io.IOException;
import java.io.InterruptedIOException;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.Map;
import java.util.concurrent.TimeUnit;
import java.util.function.Consumer;

/** Runs the external programs OCR relies on, each bounded by a timeout. */
final class Programs {

    private static final int MAX_REPORTED_ERROR_CHARS = 2000;

    /**
     * How a program ended.
     *
     * @param program       the program's name
     * @param timeoutMillis the timeout it ran under
     * @param finished      whether it ended within the timeout
     * @param status        its exit status, when it finished
     * @param output        its standard output, when it finished
     * @param errors        the end of its standard error, when it finished
     */
    record Outcome(String program, long timeoutMillis, boolean finished, int status,
                   String output, String errors) {

        boolean succeeded() {
            return finished && status == 0;
        }

        /** Why the program did not succeed, for an error message. */
        String failure() {
            if (!finished) {
                return program + " did not finish within " + timeoutMillis + " ms";
            }
            return program + " exited with status " + status
                    + (errors.isBlank() ? "" : ": " + errors.strip());
        }
    }

    private Programs() {
    }

    /**
     * Runs a command to its end or its timeout, whichever comes first; `environment` adjusts the
     * environment it runs in. Its output goes to files rather than pipes, so that a program
     * writing more than a pipe holds cannot block before the timeout.
     *
     * @throws IOException when the program cannot be started, as when it is not installed
     */
    static Outcome run(long timeoutMillis, Consumer<Map<String, String>> environment,
                       String... command) throws IOException {
        final Path output = Files.createTempFile("extractous-program-", ".out");
        try {
            final Path errors = Files.createTempFile("extractous-program-", ".err");
            try {
                return run(timeoutMillis, environment, output, errors, command);
            } finally {
                Files.deleteIfExists(errors);
            }
        } finally {
            Files.deleteIfExists(output);
        }
    }

    private static Outcome run(long timeoutMillis, Consumer<Map<String, String>> environment,
                               Path output, Path errors, String... command) throws IOException {
        final ProcessBuilder builder = new ProcessBuilder(command)
                .redirectOutput(output.toFile())
                .redirectError(errors.toFile());
        environment.accept(builder.environment());
        final Process process = builder.start();
        try {
            process.getOutputStream().close();
            if (!process.waitFor(timeoutMillis, TimeUnit.MILLISECONDS)) {
                return new Outcome(command[0], timeoutMillis, false, -1, "", "");
            }
            final String error = read(errors);
            return new Outcome(command[0], timeoutMillis, true, process.exitValue(), read(output),
                    error.substring(Math.max(0, error.length() - MAX_REPORTED_ERROR_CHARS)));
        } catch (InterruptedException e) {
            Thread.currentThread().interrupt();
            throw new InterruptedIOException("Interrupted while running " + command[0]);
        } finally {
            process.destroyForcibly();
        }
    }

    private static String read(Path file) throws IOException {
        return new String(Files.readAllBytes(file), StandardCharsets.UTF_8);
    }
}
