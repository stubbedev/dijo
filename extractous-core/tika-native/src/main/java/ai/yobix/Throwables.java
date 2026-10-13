package ai.yobix;

import java.util.Collections;
import java.util.IdentityHashMap;
import java.util.LinkedHashSet;
import java.util.Set;

/**
 * Renders throwables for the Rust side. It holds no state and loads nothing, so it still works
 * when TikaNativeMain itself failed to initialise.
 */
public final class Throwables {

    private Throwables() {
    }

    /**
     * Renders a throwable and every cause behind it as "class: message", outermost first,
     * so callers see why an operation failed and not only its outermost wrapper. A cause that
     * reads the same as one already rendered is left out: Tika rethrows the same failure once
     * per nested document, hundreds of times for a deeply nested archive.
     */
    public static String describe(Throwable throwable) {
        final Set<Throwable> seen = Collections.newSetFromMap(new IdentityHashMap<>());
        final Set<String> rendered = new LinkedHashSet<>();
        for (Throwable t = throwable; t != null && seen.add(t); t = t.getCause()) {
            rendered.add(t.getMessage() == null
                    ? t.getClass().getName()
                    : t.getClass().getName() + ": " + t.getMessage());
        }
        return String.join(" <- caused by ", rendered);
    }
}
