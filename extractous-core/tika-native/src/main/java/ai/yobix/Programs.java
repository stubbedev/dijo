package ai.yobix;

import java.io.IOException;
import java.io.InterruptedIOException;
import java.nio.charset.StandardCharsets;
import java.util.Map;
import java.util.concurrent.TimeUnit;
import java.util.function.Consumer;

/** Runs the external programs OCR relies on, each bounded by a timeout. */
final class Programs {

    private Programs() {
    }

    /**
     * The standard output of a command that succeeded in time, or null when it failed or ran too
     * long; `environment` adjusts the environment it runs in.
     *
     * @throws IOException when the program cannot be started, as when it is not installed
     */
    static String run(long timeoutMillis, Consumer<Map<String, String>> environment,
                      String... command) throws IOException {
        final ProcessBuilder builder = new ProcessBuilder(command)
                .redirectError(ProcessBuilder.Redirect.DISCARD);
        environment.accept(builder.environment());
        final Process process = builder.start();
        try {
            process.getOutputStream().close();
            if (!process.waitFor(timeoutMillis, TimeUnit.MILLISECONDS)
                    || process.exitValue() != 0) {
                return null;
            }
            return new String(process.getInputStream().readAllBytes(), StandardCharsets.UTF_8);
        } catch (InterruptedException e) {
            Thread.currentThread().interrupt();
            throw new InterruptedIOException("Interrupted while running " + command[0]);
        } finally {
            process.destroyForcibly();
        }
    }
}
