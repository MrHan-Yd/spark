using System.Runtime.InteropServices;
using System.Runtime.InteropServices.WindowsRuntime;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Media.Imaging;
using Windows.Storage.Streams;

namespace Spark.UI.Services;

/// <summary>从 exe / lnk 提取系统图标，缓存为 ImageSource。</summary>
public static class AppIconService
{
    private static readonly Dictionary<string, ImageSource?> Cache = new(StringComparer.OrdinalIgnoreCase);

    // 演示项 → 本机常见路径（找不到则回退字母色块）
    private static readonly Dictionary<string, string[]> PathCandidates = new(StringComparer.OrdinalIgnoreCase)
    {
        ["app.wt"] =
        [
            Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData),
                @"Microsoft\WindowsApps\wt.exe"),
            Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.ProgramFiles),
                @"Windows Terminal\wt.exe"),
        ],
        ["app.code"] =
        [
            Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData),
                @"Programs\Microsoft VS Code\Code.exe"),
            Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.ProgramFiles),
                @"Microsoft VS Code\Code.exe"),
        ],
        ["app.chrome"] =
        [
            Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.ProgramFiles),
                @"Google\Chrome\Application\chrome.exe"),
            Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.ProgramFilesX86),
                @"Google\Chrome\Application\chrome.exe"),
            Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData),
                @"Google\Chrome\Application\chrome.exe"),
        ],
        ["app.explorer"] =
        [
            Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.Windows), "explorer.exe"),
        ],
        ["sys.settings"] =
        [
            Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.System), "control.exe"),
            Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.System), "SystemSettingsAdminFlows.exe"),
        ],
        ["sys.lock"] =
        [
            Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.System), "shell32.dll"),
        ],
        ["file.readme"] =
        [
            Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.System), "shell32.dll"),
        ],
        ["file.arch"] =
        [
            Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.System), "shell32.dll"),
        ],
        ["tool.calc"] =
        [
            Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.System), "calc.exe"),
            Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.Windows),
                @"SystemApps\Microsoft.WindowsCalculator_8wekyb3d8bbwe\CalculatorApp.exe"),
        ],
    };

    // shell32.dll 里部分固定图标索引（文件/设置等兜底）
    private static readonly Dictionary<string, (string path, int index)> ShellIndex = new(StringComparer.OrdinalIgnoreCase)
    {
        ["file.readme"] = (Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.System), "shell32.dll"), 70),
        ["file.arch"] = (Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.System), "shell32.dll"), 70),
        ["sys.settings"] = (Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.System), "shell32.dll"), 21),
        ["sys.lock"] = (Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.System), "shell32.dll"), 47),
        ["plugin.echo"] = (Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.System), "shell32.dll"), 13),
        ["plugin.json"] = (Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.System), "shell32.dll"), 71),
        ["web.g"] = (Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.System), "shell32.dll"), 13),
    };

    public static ImageSource? GetIcon(string itemId, string? pathHint = null)
    {
        var cacheKey = string.IsNullOrEmpty(pathHint) ? itemId : itemId + "|" + pathHint;
        if (Cache.TryGetValue(cacheKey, out var cached))
            return cached;

        ImageSource? src = null;
        try
        {
            // Prefer explicit target / icon path from Host
            if (!string.IsNullOrWhiteSpace(pathHint))
            {
                var p = pathHint.Trim();
                if (File.Exists(p) || p.EndsWith(".lnk", StringComparison.OrdinalIgnoreCase))
                    src = ExtractFromFile(p, 0);
            }

            if (src is null && ShellIndex.TryGetValue(itemId, out var shell))
            {
                src = ExtractFromFile(shell.path, shell.index);
            }

            if (src is null && PathCandidates.TryGetValue(itemId, out var paths))
            {
                foreach (var p in paths)
                {
                    if (!File.Exists(p) && !p.Contains("WindowsApps", StringComparison.OrdinalIgnoreCase))
                        continue;
                    src = ExtractFromFile(p, 0);
                    if (src is not null) break;
                }
            }

            if (src is null)
            {
                var resolved = ResolveFromStartMenu(itemId);
                if (resolved is not null)
                    src = ExtractFromFile(resolved, 0);
            }
        }
        catch
        {
            src = null;
        }

        Cache[cacheKey] = src;
        return src;
    }

    private static string? ResolveFromStartMenu(string itemId)
    {
        var keywords = itemId switch
        {
            "app.wt" => new[] { "Terminal", "Windows Terminal" },
            "app.code" => new[] { "Visual Studio Code", "Code" },
            "app.chrome" => new[] { "Google Chrome", "Chrome" },
            "app.explorer" => new[] { "File Explorer", "资源管理器" },
            "tool.calc" => new[] { "Calculator", "计算器" },
            _ => Array.Empty<string>()
        };
        if (keywords.Length == 0) return null;

        var roots = new[]
        {
            Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.StartMenu), "Programs"),
            Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.CommonStartMenu), "Programs"),
            Environment.GetFolderPath(Environment.SpecialFolder.DesktopDirectory),
        };

        foreach (var root in roots)
        {
            if (!Directory.Exists(root)) continue;
            foreach (var lnk in Directory.EnumerateFiles(root, "*.lnk", SearchOption.AllDirectories))
            {
                var name = Path.GetFileNameWithoutExtension(lnk);
                if (!keywords.Any(k => name.Contains(k, StringComparison.OrdinalIgnoreCase)))
                    continue;
                var target = ResolveLnkTarget(lnk);
                if (!string.IsNullOrEmpty(target) && File.Exists(target))
                    return target;
            }
        }
        return null;
    }

    private static string? ResolveLnkTarget(string lnkPath)
    {
        try
        {
            var shellType = Type.GetTypeFromProgID("WScript.Shell");
            if (shellType is null) return null;
            dynamic shell = Activator.CreateInstance(shellType)!;
            dynamic sc = shell.CreateShortcut(lnkPath);
            string? target = sc.TargetPath as string;
            Marshal.FinalReleaseComObject(sc);
            Marshal.FinalReleaseComObject(shell);
            return target;
        }
        catch
        {
            return null;
        }
    }

    private static ImageSource? ExtractFromFile(string path, int index)
    {
        try
        {
            // 优先 SHGetFileInfo（对 lnk / 关联图标更好）
            if (index == 0 && File.Exists(path))
            {
                var fromShfi = FromShellFileInfo(path);
                if (fromShfi is not null) return fromShfi;
            }

            var large = IntPtr.Zero;
            var small = IntPtr.Zero;
            var count = ExtractIconEx(path, index, ref large, ref small, 1);
            if (count == 0 || large == IntPtr.Zero)
            {
                if (small != IntPtr.Zero) DestroyIcon(small);
                // index 失败时试 0
                if (index != 0)
                {
                    large = IntPtr.Zero;
                    small = IntPtr.Zero;
                    count = ExtractIconEx(path, 0, ref large, ref small, 1);
                }
            }

            if (large == IntPtr.Zero && small != IntPtr.Zero)
                large = small;
            else if (small != IntPtr.Zero && small != large)
                DestroyIcon(small);

            if (large == IntPtr.Zero) return null;

            try
            {
                return HiconToImageSource(large, 48);
            }
            finally
            {
                DestroyIcon(large);
            }
        }
        catch
        {
            return null;
        }
    }

    private static ImageSource? FromShellFileInfo(string path)
    {
        var shfi = new SHFILEINFO();
        var hr = SHGetFileInfo(path, 0, ref shfi, (uint)Marshal.SizeOf<SHFILEINFO>(),
            SHGFI_ICON | SHGFI_LARGEICON);
        if (hr == IntPtr.Zero || shfi.hIcon == IntPtr.Zero) return null;
        try
        {
            return HiconToImageSource(shfi.hIcon, 48);
        }
        finally
        {
            DestroyIcon(shfi.hIcon);
        }
    }

    private static ImageSource? HiconToImageSource(IntPtr hIcon, int size)
    {
        // 通过 GDI GetIconInfo + GetDIBits 转 PNG 字节，再 BitmapImage
        if (!GetIconInfo(hIcon, out var ii)) return null;
        try
        {
            var hdc = CreateCompatibleDC(IntPtr.Zero);
            if (hdc == IntPtr.Zero) return null;
            try
            {
                var bmp = ii.hbmColor != IntPtr.Zero ? ii.hbmColor : ii.hbmMask;
                if (bmp == IntPtr.Zero) return null;

                var bib = new BITMAPINFO();
                bib.bmiHeader.biSize = (uint)Marshal.SizeOf<BITMAPINFOHEADER>();
                if (GetDIBits(hdc, bmp, 0, 0, IntPtr.Zero, ref bib, DIB_RGB_COLORS) == 0)
                    return null;

                var w = Math.Abs(bib.bmiHeader.biWidth);
                var h = Math.Abs(bib.bmiHeader.biHeight);
                if (w <= 0 || h <= 0) return null;

                bib.bmiHeader.biCompression = BI_RGB;
                bib.bmiHeader.biHeight = -h; // top-down
                bib.bmiHeader.biBitCount = 32;
                bib.bmiHeader.biSizeImage = (uint)(w * h * 4);

                var buf = new byte[w * h * 4];
                var handle = GCHandle.Alloc(buf, GCHandleType.Pinned);
                try
                {
                    if (GetDIBits(hdc, bmp, 0, (uint)h, handle.AddrOfPinnedObject(), ref bib, DIB_RGB_COLORS) == 0)
                        return null;
                }
                finally
                {
                    handle.Free();
                }

                // BGRA → 预乘/修正 alpha（部分图标 alpha 全 0）
                for (var i = 0; i < buf.Length; i += 4)
                {
                    var b = buf[i];
                    var g = buf[i + 1];
                    var r = buf[i + 2];
                    var a = buf[i + 3];
                    if (a == 0 && (r | g | b) != 0) a = 255;
                    buf[i] = b;
                    buf[i + 1] = g;
                    buf[i + 2] = r;
                    buf[i + 3] = a;
                }

                return BgraToBitmapImage(buf, w, h);
            }
            finally
            {
                DeleteDC(hdc);
            }
        }
        finally
        {
            if (ii.hbmColor != IntPtr.Zero) DeleteObject(ii.hbmColor);
            if (ii.hbmMask != IntPtr.Zero) DeleteObject(ii.hbmMask);
        }
    }

    private static ImageSource? BgraToBitmapImage(byte[] bgra, int width, int height)
    {
        try
        {
            // 同步写 PNG 到内存，避免 async 死锁
            using var ms = new MemoryStream();
            WritePng(ms, bgra, width, height);
            ms.Position = 0;
            using var ras = new InMemoryRandomAccessStream();
            var writer = ras.AsStreamForWrite();
            ms.CopyTo(writer);
            writer.Flush();
            ras.Seek(0);
            var bmp = new BitmapImage();
            bmp.SetSource(ras);
            return bmp;
        }
        catch
        {
            return null;
        }
    }

    /// <summary>极简 PNG 写入（RGBA 已是 BGRA 字节，按行写入）。</summary>
    private static void WritePng(Stream stream, byte[] bgra, int width, int height)
    {
        // 转成 RGBA 并逐行 filter0
        var raw = new byte[(width * 4 + 1) * height];
        for (var y = 0; y < height; y++)
        {
            raw[y * (width * 4 + 1)] = 0; // filter none
            for (var x = 0; x < width; x++)
            {
                var si = (y * width + x) * 4;
                var di = y * (width * 4 + 1) + 1 + x * 4;
                raw[di] = bgra[si + 2];     // R
                raw[di + 1] = bgra[si + 1]; // G
                raw[di + 2] = bgra[si];     // B
                raw[di + 3] = bgra[si + 3]; // A
            }
        }

        using var ms = new MemoryStream();
        void Be32(uint v)
        {
            ms.WriteByte((byte)(v >> 24));
            ms.WriteByte((byte)(v >> 16));
            ms.WriteByte((byte)(v >> 8));
            ms.WriteByte((byte)v);
        }
        void Chunk(string type, byte[] data)
        {
            Be32((uint)data.Length);
            var typeBytes = System.Text.Encoding.ASCII.GetBytes(type);
            ms.Write(typeBytes, 0, 4);
            ms.Write(data, 0, data.Length);
            var crc = Crc32(typeBytes, data);
            Be32(crc);
        }

        // signature
        ms.Write(new byte[] { 137, 80, 78, 71, 13, 10, 26, 10 }, 0, 8);
        // IHDR
        using (var ihdr = new MemoryStream())
        {
            void W32(uint v) { ihdr.WriteByte((byte)(v >> 24)); ihdr.WriteByte((byte)(v >> 16)); ihdr.WriteByte((byte)(v >> 8)); ihdr.WriteByte((byte)v); }
            W32((uint)width); W32((uint)height);
            ihdr.WriteByte(8); ihdr.WriteByte(6); ihdr.WriteByte(0); ihdr.WriteByte(0); ihdr.WriteByte(0);
            Chunk("IHDR", ihdr.ToArray());
        }
        // IDAT (zlib)
        byte[] idat;
        using (var comp = new MemoryStream())
        {
            // zlib header
            comp.WriteByte(0x78); comp.WriteByte(0x01);
            using (var ds = new System.IO.Compression.DeflateStream(comp, System.IO.Compression.CompressionLevel.Fastest, true))
                ds.Write(raw, 0, raw.Length);
            // adler32
            var adler = Adler32(raw);
            comp.WriteByte((byte)(adler >> 24));
            comp.WriteByte((byte)(adler >> 16));
            comp.WriteByte((byte)(adler >> 8));
            comp.WriteByte((byte)adler);
            idat = comp.ToArray();
        }
        Chunk("IDAT", idat);
        Chunk("IEND", Array.Empty<byte>());
        ms.Position = 0;
        ms.CopyTo(stream);
    }

    private static uint Crc32(byte[] type, byte[] data)
    {
        uint crc = 0xFFFFFFFF;
        void Feed(byte b)
        {
            crc ^= b;
            for (var k = 0; k < 8; k++)
                crc = (crc & 1) != 0 ? (crc >> 1) ^ 0xEDB88320u : crc >> 1;
        }
        foreach (var b in type) Feed(b);
        foreach (var b in data) Feed(b);
        return crc ^ 0xFFFFFFFF;
    }

    private static uint Adler32(byte[] data)
    {
        uint a = 1, b = 0;
        foreach (var d in data)
        {
            a = (a + d) % 65521;
            b = (b + a) % 65521;
        }
        return (b << 16) | a;
    }

    #region P/Invoke

    private const uint SHGFI_ICON = 0x000000100;
    private const uint SHGFI_LARGEICON = 0x000000000;
    private const int DIB_RGB_COLORS = 0;
    private const int BI_RGB = 0;

    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    private struct SHFILEINFO
    {
        public IntPtr hIcon;
        public int iIcon;
        public uint dwAttributes;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 260)]
        public string szDisplayName;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 80)]
        public string szTypeName;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct ICONINFO
    {
        public bool fIcon;
        public int xHotspot;
        public int yHotspot;
        public IntPtr hbmMask;
        public IntPtr hbmColor;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct BITMAPINFOHEADER
    {
        public uint biSize;
        public int biWidth;
        public int biHeight;
        public ushort biPlanes;
        public ushort biBitCount;
        public uint biCompression;
        public uint biSizeImage;
        public int biXPelsPerMeter;
        public int biYPelsPerMeter;
        public uint biClrUsed;
        public uint biClrImportant;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct BITMAPINFO
    {
        public BITMAPINFOHEADER bmiHeader;
        public uint bmiColors;
    }

    [DllImport("shell32.dll", CharSet = CharSet.Unicode)]
    private static extern IntPtr SHGetFileInfo(string pszPath, uint dwFileAttributes,
        ref SHFILEINFO psfi, uint cbFileInfo, uint uFlags);

    [DllImport("shell32.dll", CharSet = CharSet.Unicode)]
    private static extern uint ExtractIconEx(string lpszFile, int nIconIndex,
        ref IntPtr phiconLarge, ref IntPtr phiconSmall, uint nIcons);

    [DllImport("user32.dll", SetLastError = true)]
    private static extern bool DestroyIcon(IntPtr hIcon);

    [DllImport("user32.dll")]
    private static extern bool GetIconInfo(IntPtr hIcon, out ICONINFO piconinfo);

    [DllImport("gdi32.dll")]
    private static extern IntPtr CreateCompatibleDC(IntPtr hdc);

    [DllImport("gdi32.dll")]
    private static extern bool DeleteDC(IntPtr hdc);

    [DllImport("gdi32.dll")]
    private static extern bool DeleteObject(IntPtr hObject);

    [DllImport("gdi32.dll")]
    private static extern int GetDIBits(IntPtr hdc, IntPtr hbmp, uint uStartScan, uint cScanLines,
        IntPtr lpvBits, ref BITMAPINFO lpbi, uint uUsage);

    #endregion
}
