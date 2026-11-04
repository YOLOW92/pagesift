package dev.pagesift;

import java.lang.foreign.Arena;
import java.lang.foreign.FunctionDescriptor;
import java.lang.foreign.Linker;
import java.lang.foreign.MemorySegment;
import java.lang.foreign.SymbolLookup;
import java.lang.invoke.MethodHandle;
import java.io.IOException;
import java.io.InputStream;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.StandardCopyOption;
import java.util.Locale;

import static java.lang.foreign.ValueLayout.ADDRESS;
import static java.lang.foreign.ValueLayout.JAVA_BYTE;
import static java.lang.foreign.ValueLayout.JAVA_LONG;

/**
 * libpagesift 的 Java 封装，使用 JDK 内置的 FFM API（java.lang.foreign，JDK 22 起正式版）。
 *
 * <p>库的加载顺序：
 * <ol>
 *   <li>系统属性 {@code pagesift.lib} 或环境变量 {@code PAGESIFT_LIB}（库文件的完整路径）；</li>
 *   <li>jar 里自带的库（{@code META-INF/native/<系统>-<架构>/}，有 linux-x86_64、linux-aarch64、macos-aarch64），
 *       解压到临时文件后加载，进程退出时删除；</li>
 *   <li>系统库名（{@code libpagesift.so} / {@code libpagesift.dylib}），由系统的动态链接器查找。</li>
 * </ol>
 * 不会联网下载。没有 Windows 版本。
 *
 * <p>方法返回的是 Document JSON 字符串（或引擎配置的错误）。这里不解析 JSON，需要的话交给自己的 JSON 库。
 * 运行时需要 {@code --enable-native-access=ALL-UNNAMED}，否则 JDK 会打印警告。
 */
public final class Pagesift {
    private Pagesift() {}

    private static final MethodHandle VERSION;
    private static final MethodHandle STRING_FREE;
    private static final MethodHandle ENGINE_NEW;
    private static final MethodHandle ENGINE_FREE;
    private static final MethodHandle CONVERT;
    private static final MethodHandle PARSE;
    private static final MethodHandle SCRAPE;

    static {
        SymbolLookup sym = openLibrary();
        Linker linker = Linker.nativeLinker();
        VERSION = bind(linker, sym, "pagesift_version", FunctionDescriptor.of(ADDRESS));
        STRING_FREE = bind(linker, sym, "pagesift_string_free", FunctionDescriptor.ofVoid(ADDRESS));
        ENGINE_NEW = bind(linker, sym, "pagesift_engine_new",
                FunctionDescriptor.of(ADDRESS, ADDRESS, ADDRESS));
        ENGINE_FREE = bind(linker, sym, "pagesift_engine_free", FunctionDescriptor.ofVoid(ADDRESS));
        CONVERT = bind(linker, sym, "pagesift_convert",
                FunctionDescriptor.of(ADDRESS, ADDRESS, ADDRESS));
        PARSE = bind(linker, sym, "pagesift_parse",
                FunctionDescriptor.of(ADDRESS, ADDRESS, ADDRESS, JAVA_LONG, ADDRESS, ADDRESS));
        SCRAPE = bind(linker, sym, "pagesift_scrape",
                FunctionDescriptor.of(ADDRESS, ADDRESS, ADDRESS, ADDRESS));
    }

    /** 当前平台的 jar 资源目录名，例如 linux-x86_64；不支持的平台返回 null。 */
    static String platformDir() {
        String os = System.getProperty("os.name", "").toLowerCase(Locale.ROOT);
        String arch = System.getProperty("os.arch", "").toLowerCase(Locale.ROOT);
        String a = (arch.equals("amd64") || arch.equals("x86_64")) ? "x86_64"
                : (arch.equals("aarch64") || arch.equals("arm64")) ? "aarch64" : null;
        if (a == null) return null;
        if (os.contains("linux")) return "linux-" + a;
        if (os.contains("mac")) return "macos-" + a;
        return null;
    }

    private static String libFileName() {
        String os = System.getProperty("os.name", "").toLowerCase(Locale.ROOT);
        return os.contains("mac") ? "libpagesift.dylib" : "libpagesift.so";
    }

    private static SymbolLookup openLibrary() {
        String p = System.getProperty("pagesift.lib");
        if (p == null || p.isEmpty()) {
            p = System.getenv("PAGESIFT_LIB");
        }
        if (p != null && !p.isEmpty()) {
            return SymbolLookup.libraryLookup(Path.of(p), Arena.global());
        }
        Path bundled = extractBundled();
        if (bundled != null) {
            return SymbolLookup.libraryLookup(bundled, Arena.global());
        }
        return SymbolLookup.libraryLookup(libFileName(), Arena.global());
    }

    /** 把 jar 里当前平台的库解压到临时文件；jar 里没有就返回 null。 */
    private static Path extractBundled() {
        String dir = platformDir();
        if (dir == null) return null;
        String res = "/META-INF/native/" + dir + "/" + libFileName();
        try (InputStream in = Pagesift.class.getResourceAsStream(res)) {
            if (in == null) return null;
            Path tmp = Files.createTempFile("libpagesift-", libFileName().substring("libpagesift".length()));
            tmp.toFile().deleteOnExit();
            Files.copy(in, tmp, StandardCopyOption.REPLACE_EXISTING);
            return tmp;
        } catch (IOException e) {
            throw new IllegalStateException("cannot extract bundled libpagesift: " + e, e);
        }
    }

    private static MethodHandle bind(Linker linker, SymbolLookup sym, String name, FunctionDescriptor fd) {
        MemorySegment addr = sym.find(name)
                .orElseThrow(() -> new IllegalStateException("libpagesift has no symbol " + name));
        return linker.downcallHandle(addr, fd);
    }

    /** 库版本号，例如 "1.0.0"。 */
    public static String version() {
        try {
            MemorySegment p = (MemorySegment) VERSION.invokeExact();
            return p.reinterpret(Long.MAX_VALUE).getString(0); // 静态字符串，不释放
        } catch (Throwable t) {
            throw wrap(t);
        }
    }

    /** 把 HTML 转成 Markdown。不需要引擎。options 为 null 时用默认值。 */
    public static String convert(String html, String optionsJson) {
        try (Arena arena = Arena.ofConfined()) {
            MemorySegment h = arena.allocateFrom(html);
            MemorySegment o = optionsJson == null ? MemorySegment.NULL : arena.allocateFrom(optionsJson);
            return take((MemorySegment) CONVERT.invokeExact(h, o));
        } catch (Throwable t) {
            throw wrap(t);
        }
    }

    /** 引擎：连接池、限速和浏览器池。可以在多个线程里共用；用完 close。 */
    public static final class Engine implements AutoCloseable {
        private MemorySegment handle;

        private Engine(MemorySegment handle) {
            this.handle = handle;
        }

        /** 建引擎。configJson 为 null 时用默认配置。失败抛 IllegalArgumentException。 */
        public static Engine create(String configJson) {
            try (Arena arena = Arena.ofConfined()) {
                MemorySegment cfg = configJson == null ? MemorySegment.NULL : arena.allocateFrom(configJson);
                MemorySegment errOut = arena.allocate(ADDRESS);
                MemorySegment e = (MemorySegment) ENGINE_NEW.invokeExact(cfg, errOut);
                if (e.equals(MemorySegment.NULL)) {
                    String msg = take(errOut.get(ADDRESS, 0));
                    throw new IllegalArgumentException(msg == null ? "engine_new failed" : msg);
                }
                return new Engine(e);
            } catch (IllegalArgumentException ex) {
                throw ex;
            } catch (Throwable t) {
                throw wrap(t);
            }
        }

        /** 解析内存里的文件（PDF、HTML、Office、CSV 等），按内容和文件名识别类型。 */
        public String parse(byte[] data, String filename, String optionsJson) {
            ensureOpen();
            try (Arena arena = Arena.ofConfined()) {
                MemorySegment d = data.length == 0 ? MemorySegment.NULL
                        : arena.allocateFrom(JAVA_BYTE, data);
                MemorySegment name = arena.allocateFrom(filename);
                MemorySegment o = optionsJson == null ? MemorySegment.NULL : arena.allocateFrom(optionsJson);
                return take((MemorySegment) PARSE.invokeExact(handle, d, (long) data.length, name, o));
            } catch (Throwable t) {
                throw wrap(t);
            }
        }

        /** 抓取一个 URL。 */
        public String scrape(String url, String optionsJson) {
            ensureOpen();
            try (Arena arena = Arena.ofConfined()) {
                MemorySegment u = arena.allocateFrom(url);
                MemorySegment o = optionsJson == null ? MemorySegment.NULL : arena.allocateFrom(optionsJson);
                return take((MemorySegment) SCRAPE.invokeExact(handle, u, o));
            } catch (Throwable t) {
                throw wrap(t);
            }
        }

        @Override
        public void close() {
            if (handle != null) {
                try {
                    ENGINE_FREE.invokeExact(handle);
                } catch (Throwable t) {
                    throw wrap(t);
                } finally {
                    handle = null;
                }
            }
        }

        private void ensureOpen() {
            if (handle == null) {
                throw new IllegalStateException("engine is closed");
            }
        }
    }

    /** 把库返回的 C 字符串复制成 Java 字符串，并释放库里的那一份。NULL 返回 null。 */
    private static String take(MemorySegment p) throws Throwable {
        if (p.equals(MemorySegment.NULL)) {
            return null;
        }
        try {
            return p.reinterpret(Long.MAX_VALUE).getString(0);
        } finally {
            STRING_FREE.invokeExact(p);
        }
    }

    private static RuntimeException wrap(Throwable t) {
        if (t instanceof RuntimeException re) {
            return re;
        }
        if (t instanceof Error err) {
            throw err;
        }
        return new IllegalStateException(t);
    }
}
