//! libpagesift 的 Zig 绑定：直接声明 C ABI（不用 @cImport），链接 libpagesift。
//! 返回 Document JSON 的切片，借用库里的内存；用完调用 `free`。

const c = struct {
    pub const Handle = opaque {};
    pub extern fn pagesift_version() [*:0]const u8;
    pub extern fn pagesift_string_free(s: [*:0]u8) void;
    pub extern fn pagesift_engine_new(config_json: ?[*:0]const u8, error_out: *?[*:0]u8) ?*Handle;
    pub extern fn pagesift_engine_free(e: *Handle) void;
    pub extern fn pagesift_scrape(e: *const Handle, url: [*:0]const u8, options_json: ?[*:0]const u8) ?[*:0]u8;
    pub extern fn pagesift_parse(e: *const Handle, data: ?[*]const u8, len: usize, filename: [*:0]const u8, options_json: ?[*:0]const u8) ?[*:0]u8;
    pub extern fn pagesift_convert(html: [*:0]const u8, options_json: ?[*:0]const u8) ?[*:0]u8;
};

pub const Error = error{ BadConfig, NoResult };

/// 库返回的一段 JSON 文本。
pub const Result = struct {
    ptr: [*:0]u8,
    pub fn text(self: Result) []const u8 {
        return span(self.ptr);
    }
    pub fn free(self: Result) void {
        c.pagesift_string_free(self.ptr);
    }
};

fn span(p: [*:0]const u8) []const u8 {
    var n: usize = 0;
    while (p[n] != 0) n += 1;
    return p[0..n];
}

pub fn version() []const u8 {
    return span(c.pagesift_version());
}

pub fn convert(html: [*:0]const u8, options_json: ?[*:0]const u8) Error!Result {
    return .{ .ptr = c.pagesift_convert(html, options_json) orelse return Error.NoResult };
}

pub const Engine = struct {
    handle: *c.Handle,

    /// 建引擎；config_json 为 null 用默认配置。配置不合法返回 error.BadConfig。
    pub fn init(config_json: ?[*:0]const u8) Error!Engine {
        var err: ?[*:0]u8 = null;
        const h = c.pagesift_engine_new(config_json, &err) orelse {
            if (err) |e| c.pagesift_string_free(e);
            return Error.BadConfig;
        };
        return .{ .handle = h };
    }

    pub fn deinit(self: Engine) void {
        c.pagesift_engine_free(self.handle);
    }

    pub fn parse(self: Engine, data: []const u8, filename: [*:0]const u8, options_json: ?[*:0]const u8) Error!Result {
        const p = if (data.len == 0) null else data.ptr;
        return .{ .ptr = c.pagesift_parse(self.handle, p, data.len, filename, options_json) orelse return Error.NoResult };
    }

    pub fn scrape(self: Engine, url: [*:0]const u8, options_json: ?[*:0]const u8) Error!Result {
        return .{ .ptr = c.pagesift_scrape(self.handle, url, options_json) orelse return Error.NoResult };
    }
};
