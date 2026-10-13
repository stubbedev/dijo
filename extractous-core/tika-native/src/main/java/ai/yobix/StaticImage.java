package ai.yobix;

import org.graalvm.nativeimage.RuntimeOptions;
import org.graalvm.nativeimage.UnmanagedMemory;
import org.graalvm.nativeimage.c.function.CFunction;
import org.graalvm.nativeimage.c.struct.SizeOf;
import org.graalvm.nativeimage.c.type.CCharPointerPointer;
import org.graalvm.nativeimage.c.type.CTypeConversion;
import org.graalvm.nativeimage.c.type.CTypeConversion.CCharPointerHolder;
import org.graalvm.word.WordFactory;

/**
 * The entry point of the statically linked Linux executable, in which the extractous program is
 * linked into this native image: Java starts first, as a native image does, and hands the command
 * line to the program, which then calls Tika through JNI in this same Java VM.
 */
public final class StaticImage {

    private static final String PROGRAM = "extractous";

    private StaticImage() {
    }

    @CFunction("extractous_main")
    private static native int extractousMain(int argc, CCharPointerPointer argv);

    public static void main(String[] args) {
        final CCharPointerHolder[] holders = new CCharPointerHolder[args.length + 1];
        final CCharPointerPointer argv = UnmanagedMemory.malloc(
                (args.length + 2) * SizeOf.get(CCharPointerPointer.class));
        holders[0] = CTypeConversion.toCString(PROGRAM);
        for (int index = 0; index < args.length; index++) {
            holders[index + 1] = CTypeConversion.toCString(args[index]);
        }
        for (int index = 0; index < holders.length; index++) {
            argv.write(index, holders[index].get());
        }
        argv.write(holders.length, WordFactory.nullPointer());
        System.exit(extractousMain(holders.length, argv));
    }

    /** Sets a system property, as the program does before the first extraction. */
    static void setProperty(String key, String value) {
        System.setProperty(key, value);
    }

    /** Caps the Java heap, as `-XX:MaxHeapSize` does where the program starts the Java VM. */
    static void setMaxHeapBytes(long bytes) {
        RuntimeOptions.set("MaxHeapSize", bytes);
    }

    /** Sets the heap kept at least, as `-XX:MinHeapSize` does where the program starts the VM. */
    static void setMinHeapBytes(long bytes) {
        RuntimeOptions.set("MinHeapSize", bytes);
    }
}
