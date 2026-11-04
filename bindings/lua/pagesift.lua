-- libpagesift 的 LuaJIT 绑定（LuaJIT 自带的 ffi，无第三方依赖）。
-- 函数返回 Document JSON 字符串，不解析。
-- 库的加载顺序：环境变量 PAGESIFT_LIB（库文件完整路径）；然后系统库名 pagesift
-- （ffi.load 会补成 libpagesift.so / libpagesift.dylib，由动态链接器按 LD_LIBRARY_PATH、ldconfig 等查找）。
-- 包里不带库，请安装 GitHub Release 里的 C 库；不会联网下载。
local ffi = require("ffi")

ffi.cdef [[
typedef struct PagesiftEngine pagesift_engine;
const char *pagesift_version(void);
void pagesift_string_free(char *s);
pagesift_engine *pagesift_engine_new(const char *config_json, char **error_out);
void pagesift_engine_free(pagesift_engine *engine);
char *pagesift_scrape(const pagesift_engine *engine, const char *url, const char *options_json);
char *pagesift_parse(const pagesift_engine *engine, const uint8_t *data, size_t len,
                     const char *filename, const char *options_json);
char *pagesift_convert(const char *html, const char *options_json);
]]

local lib_path = os.getenv("PAGESIFT_LIB")
if lib_path == nil or lib_path == "" then lib_path = "pagesift" end
local ok, C = pcall(ffi.load, lib_path)
if not ok then
  error("cannot load libpagesift (" .. lib_path .. "): " .. tostring(C) ..
    "\ninstall the C library from the GitHub Release and put it on the library path, or set PAGESIFT_LIB", 0)
end

local M = {}

-- 把库返回的 char* 复制成 Lua 字符串，并释放库里的那一份。
local function take(p)
  if p == nil then return nil end
  local s = ffi.string(p)
  C.pagesift_string_free(p)
  return s
end

function M.version() return ffi.string(C.pagesift_version()) end

function M.convert(html, options_json)
  return take(C.pagesift_convert(html, options_json))
end

local Engine = {}
Engine.__index = Engine

-- 建引擎；config_json 为 nil 用默认配置。失败时抛错（error 值是库给的原因）。
function M.engine(config_json)
  local err = ffi.new("char *[1]")
  local e = C.pagesift_engine_new(config_json, err)
  if e == nil then
    local msg = take(err[0])
    error(msg or "engine_new failed", 2)
  end
  return setmetatable({ h = ffi.gc(e, C.pagesift_engine_free) }, Engine)
end

function Engine:close()
  if self.h then
    ffi.gc(self.h, nil)
    C.pagesift_engine_free(self.h)
    self.h = nil
  end
end

local function live(self)
  if not self.h then error("engine is closed", 3) end
  return self.h
end

function Engine:parse(data, filename, options_json)
  return take(C.pagesift_parse(live(self), data, #data, filename, options_json))
end

function Engine:scrape(url, options_json)
  return take(C.pagesift_scrape(live(self), url, options_json))
end

return M
