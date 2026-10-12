package ai.yobix;

import java.util.Collections;
import java.util.IdentityHashMap;
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
     * so callers see why an operation failed and not only its outermost wrapper.
     */
    public static String describe(Throwable throwable) {
        final StringBuilder description = new StringBuilder();
        final Set<Throwable> seen = Collections.newSetFromMap(new IdentityHashMap<>());
        for (Throwable t = throwable; t != null && seen.add(t); t = t.getCause()) {
            if (description.length() > 0) {
                description.append(" <- caused by ");
            }
            description.append(t.getClass().getName());
            if (t.getMessage() != null) {
                description.append(": ").append(t.getMessage());
            }
        }
        return description.toString();
    }
}
