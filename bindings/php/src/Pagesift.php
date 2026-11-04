<?php

declare(strict_types=1);

namespace Pagesift;

use FFI;
use FFI\CData;
use InvalidArgumentException;
use RuntimeException;


/**
 * libpagesift 的 PHP 封装，用 ext-ffi（PHP 自带的 FFI 扩展）调用 C ABI。没有 Composer 依赖。
 *
 * 方法返回 Document JSON 字符串，不解析。需要 ffi.enable=1（CLI 默认开启）。
 * 库的加载顺序：环境变量 PAGESIFT_LIB（库文件完整路径）；然后系统库名（libpagesift.so / libpagesift.dylib，
 * 由动态链接器按 LD_LIBRARY_PATH、ldconfig 等查找）。包里不带库，请安装 GitHub Release 里的 C 库；不会联网下载。
 */
final class Pagesift
{
    private const CDEF = <<<'C'
typedef struct PagesiftEngine PagesiftEngine;
const void *pagesift_version(void);
/* 返回值声明成 void*：char* 会被 FFI 自动转成 PHP 字符串，拿不到指针就没法释放。 */
void pagesift_string_free(void *s);
PagesiftEngine *pagesift_engine_new(const char *config_json, char **error_out);
void pagesift_engine_free(PagesiftEngine *engine);
void *pagesift_convert(const char *html, const char *options_json);
void *pagesift_parse(PagesiftEngine *engine, const uint8_t *data, size_t len,
                     const char *filename, const char *options_json);
void *pagesift_scrape(PagesiftEngine *engine, const char *url, const char *options_json);
C;

    private static ?FFI $ffi = null;

    /** 共享的 FFI 句柄，第一次使用时加载库。 */
    public static function lib(): FFI
    {
        if (self::$ffi === null) {
            $path = getenv('PAGESIFT_LIB');
            if ($path !== false && $path !== '') {
                if (!is_file($path)) {
                    throw new RuntimeException("libpagesift not found at {$path} (PAGESIFT_LIB)");
                }
            } else {
                $path = PHP_OS_FAMILY === 'Darwin' ? 'libpagesift.dylib' : 'libpagesift.so';
            }
            try {
                self::$ffi = FFI::cdef(self::CDEF, $path);
            } catch (\FFI\Exception $e) {
                throw new RuntimeException(
                    "cannot load {$path}: {$e->getMessage()} (install the C library from the GitHub Release "
                    . 'and put it on the library path, or set PAGESIFT_LIB)',
                    0,
                    $e
                );
            }
        }
        return self::$ffi;
    }

    /** 把库返回的 char* 复制成 PHP 字符串，并释放库里的那一份。NULL 返回 null。 */
    public static function take(?CData $p): ?string
    {
        if ($p === null || FFI::isNull($p)) {
            return null;
        }
        try {
            return FFI::string(self::lib()->cast('char *', $p));
        } finally {
            self::lib()->pagesift_string_free($p);
        }
    }

    /** 库版本号，例如 "1.0.0"。 */
    public static function version(): string
    {
        return FFI::string(self::lib()->cast('char *', self::lib()->pagesift_version())); // 静态字符串，不释放
    }

    /** HTML 转 Markdown，不需要引擎。options 为 null 时用默认值。 */
    public static function convert(string $html, ?string $optionsJson = null): string
    {
        return self::take(self::lib()->pagesift_convert($html, $optionsJson)) ?? '';
    }
}

/** 引擎：连接池、限速和浏览器池。用完调用 close()，或让对象析构。 */
final class Engine
{
    private ?CData $handle;

    private function __construct(CData $handle)
    {
        $this->handle = $handle;
    }

    /** 建引擎。configJson 为 null 时用默认配置。失败抛 InvalidArgumentException。 */
    public static function create(?string $configJson = null): self
    {
        $lib = Pagesift::lib();
        $errOut = $lib->new('char*');
        $handle = $lib->pagesift_engine_new($configJson, FFI::addr($errOut));
        if ($handle === null || FFI::isNull($handle)) {
            $msg = Pagesift::take($errOut);
            throw new InvalidArgumentException($msg ?? 'engine_new failed');
        }
        return new self($handle);
    }

    /** 解析内存里的文件（PDF、HTML、Office、CSV 等），按内容和文件名识别类型。 */
    public function parse(string $data, string $filename, ?string $optionsJson = null): string
    {
        $handle = $this->live();
        $lib = Pagesift::lib();
        $len = strlen($data);
        $buf = null;
        if ($len > 0) {
            $buf = $lib->new("uint8_t[{$len}]", false);
            FFI::memcpy($buf, $data, $len);
        }
        return Pagesift::take($lib->pagesift_parse($handle, $buf, $len, $filename, $optionsJson)) ?? '';
    }

    /** 抓取一个 URL。 */
    public function scrape(string $url, ?string $optionsJson = null): string
    {
        return Pagesift::take(Pagesift::lib()->pagesift_scrape($this->live(), $url, $optionsJson)) ?? '';
    }

    public function close(): void
    {
        if ($this->handle !== null) {
            Pagesift::lib()->pagesift_engine_free($this->handle);
            $this->handle = null;
        }
    }

    public function __destruct()
    {
        $this->close();
    }

    private function live(): CData
    {
        if ($this->handle === null) {
            throw new RuntimeException('engine is closed');
        }
        return $this->handle;
    }
}
