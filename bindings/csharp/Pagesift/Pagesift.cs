using System.Reflection;
using System.Runtime.InteropServices;

namespace PagesiftLib;

/// <summary>
/// libpagesift 的 P/Invoke 封装。方法返回 Document JSON 字符串，不解析。
/// 库的加载顺序：环境变量 PAGESIFT_LIB（库文件完整路径）；然后包内自带的库
/// （runtimes/&lt;rid&gt;/native，有 linux-x64、linux-arm64、osx-arm64）；最后是系统库名（LD_LIBRARY_PATH、应用目录等）。不联网下载。
/// </summary>
public static class Pagesift
{
    private const string Lib = "pagesift";

    static Pagesift()
    {
        NativeLibrary.SetDllImportResolver(typeof(Pagesift).Assembly, Resolve);
    }

    private static IntPtr Resolve(string name, Assembly assembly, DllImportSearchPath? path)
    {
        if (name != Lib)
        {
            return IntPtr.Zero;
        }
        var explicitPath = Environment.GetEnvironmentVariable("PAGESIFT_LIB");
        if (!string.IsNullOrEmpty(explicitPath) && NativeLibrary.TryLoad(explicitPath, out var h))
        {
            return h;
        }
        // NuGet 包里的 runtimes/<rid>/native。正常情况下 .NET 按 deps.json 自己能找到，这里再显式找一次，
        // 为的是没有 deps.json 的发布方式（例如直接拷贝 dll）。
        var rid = CurrentRid();
        if (rid != null)
        {
            var file = OperatingSystem.IsMacOS() ? "libpagesift.dylib" : "libpagesift.so";
            var bundled = Path.Combine(AppContext.BaseDirectory, "runtimes", rid, "native", file);
            if (File.Exists(bundled) && NativeLibrary.TryLoad(bundled, out var h3))
            {
                return h3;
            }
        }
        return NativeLibrary.TryLoad(Lib, assembly, path, out var h2) ? h2 : IntPtr.Zero;
    }

    private static string? CurrentRid()
    {
        var arch = RuntimeInformation.OSArchitecture switch
        {
            Architecture.X64 => "x64",
            Architecture.Arm64 => "arm64",
            _ => null,
        };
        if (arch == null) return null;
        if (OperatingSystem.IsLinux()) return "linux-" + arch;
        if (OperatingSystem.IsMacOS()) return "osx-" + arch;
        return null;
    }

    [DllImport(Lib, CallingConvention = CallingConvention.Cdecl)]
    private static extern IntPtr pagesift_version();

    [DllImport(Lib, CallingConvention = CallingConvention.Cdecl)]
    private static extern void pagesift_string_free(IntPtr s);

    [DllImport(Lib, CallingConvention = CallingConvention.Cdecl)]
    private static extern IntPtr pagesift_engine_new(IntPtr configJson, out IntPtr errorOut);

    [DllImport(Lib, CallingConvention = CallingConvention.Cdecl)]
    private static extern void pagesift_engine_free(IntPtr engine);

    [DllImport(Lib, CallingConvention = CallingConvention.Cdecl)]
    private static extern IntPtr pagesift_convert(IntPtr html, IntPtr optionsJson);

    [DllImport(Lib, CallingConvention = CallingConvention.Cdecl)]
    private static extern IntPtr pagesift_parse(IntPtr engine, IntPtr data, UIntPtr len,
        IntPtr filename, IntPtr optionsJson);

    [DllImport(Lib, CallingConvention = CallingConvention.Cdecl)]
    private static extern IntPtr pagesift_scrape(IntPtr engine, IntPtr url, IntPtr optionsJson);

    /// <summary>库版本号，例如 "1.0.0"。</summary>
    public static string Version()
    {
        // 静态字符串，不释放
        return Marshal.PtrToStringUTF8(pagesift_version()) ?? "";
    }

    /// <summary>把 HTML 转成 Markdown。不需要引擎。optionsJson 为 null 时用默认值。</summary>
    public static string Convert(string html, string? optionsJson = null)
    {
        using var h = Native.Utf8(html);
        using var o = Native.Utf8(optionsJson);
        return Native.Take(pagesift_convert(h.Ptr, o.Ptr));
    }

    /// <summary>引擎：连接池、限速和浏览器池。可以在多个线程里共用；用完 Dispose。</summary>
    public sealed class Engine : IDisposable
    {
        private IntPtr _handle;

        private Engine(IntPtr handle)
        {
            _handle = handle;
        }

        /// <summary>建引擎。configJson 为 null 时用默认配置。失败抛 ArgumentException。</summary>
        public static Engine Create(string? configJson = null)
        {
            using var cfg = Native.Utf8(configJson);
            var handle = pagesift_engine_new(cfg.Ptr, out var errOut);
            if (handle == IntPtr.Zero)
            {
                var msg = Native.Take(errOut);
                throw new ArgumentException(string.IsNullOrEmpty(msg) ? "engine_new failed" : msg);
            }
            return new Engine(handle);
        }

        /// <summary>解析内存里的文件（PDF、HTML、Office、CSV 等），按内容和文件名识别类型。</summary>
        public string Parse(byte[] data, string filename, string? optionsJson = null)
        {
            var handle = Live();
            var pin = GCHandle.Alloc(data, GCHandleType.Pinned);
            try
            {
                using var name = Native.Utf8(filename);
                using var o = Native.Utf8(optionsJson);
                var ptr = data.Length == 0 ? IntPtr.Zero : pin.AddrOfPinnedObject();
                return Native.Take(pagesift_parse(handle, ptr, (UIntPtr)data.Length, name.Ptr, o.Ptr));
            }
            finally
            {
                pin.Free();
            }
        }

        /// <summary>抓取一个 URL。</summary>
        public string Scrape(string url, string? optionsJson = null)
        {
            var handle = Live();
            using var u = Native.Utf8(url);
            using var o = Native.Utf8(optionsJson);
            return Native.Take(pagesift_scrape(handle, u.Ptr, o.Ptr));
        }

        public void Dispose()
        {
            if (_handle != IntPtr.Zero)
            {
                pagesift_engine_free(_handle);
                _handle = IntPtr.Zero;
            }
        }

        private IntPtr Live()
        {
            if (_handle == IntPtr.Zero)
            {
                throw new ObjectDisposedException(nameof(Engine));
            }
            return _handle;
        }
    }

    /// <summary>非托管字符串的临时副本（UTF-8，NUL 结尾），Dispose 时释放。null 得到 IntPtr.Zero。</summary>
    private readonly struct Native : IDisposable
    {
        public readonly IntPtr Ptr;
        private readonly bool _owned;

        private Native(IntPtr ptr, bool owned)
        {
            Ptr = ptr;
            _owned = owned;
        }

        public static Native Utf8(string? s)
        {
            return s is null ? new Native(IntPtr.Zero, false) : new Native(Marshal.StringToCoTaskMemUTF8(s), true);
        }

        /// <summary>把库返回的字符串复制成托管字符串，并释放库里的那一份。</summary>
        public static string Take(IntPtr p)
        {
            if (p == IntPtr.Zero)
            {
                return "";
            }
            try
            {
                return Marshal.PtrToStringUTF8(p) ?? "";
            }
            finally
            {
                pagesift_string_free(p);
            }
        }

        public void Dispose()
        {
            if (_owned)
            {
                Marshal.FreeCoTaskMem(Ptr);
            }
        }
    }
}
