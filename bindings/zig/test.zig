const std = @import("std");
const ps = @import("pagesift.zig");
const t = std.testing;

fn has(hay: []const u8, needle: []const u8) bool {
    return std.mem.indexOf(u8, hay, needle) != null;
}

test "version looks like x.y.z" {
    var dots: usize = 0;
    for (ps.version()) |ch| {
        if (ch == '.') dots += 1;
    }
    try t.expectEqual(@as(usize, 2), dots);
}

test "convert html" {
    const r = try ps.convert("<h1>Hi</h1><p>World</p>", null);
    defer r.free();
    try t.expect(has(r.text(), "\"success\":true"));
    try t.expect(has(r.text(), "# Hi"));
}

test "parse csv bytes" {
    const e = try ps.Engine.init(null);
    defer e.deinit();
    const r = try e.parse("name,score\nalice,1\nbob,2\n", "scores.csv", null);
    defer r.free();
    try t.expect(has(r.text(), "\"success\":true"));
    try t.expect(has(r.text(), "| alice | 1 |"));
}

test "unsupported file is reported" {
    const e = try ps.Engine.init(null);
    defer e.deinit();
    const r = try e.parse(&[_]u8{ 0, 1, 2, 3 }, "blob.xyz", null);
    defer r.free();
    try t.expect(has(r.text(), "\"success\":false"));
    try t.expect(has(r.text(), "\"error_code\":\"UNSUPPORTED_OPTION\""));
}

test "bad url is reported" {
    const e = try ps.Engine.init(null);
    defer e.deinit();
    const r = try e.scrape("not a url", null);
    defer r.free();
    try t.expect(has(r.text(), "\"error_code\":\"INVALID_REQUEST\""));
}

test "bad engine config is rejected" {
    try t.expectError(ps.Error.BadConfig, ps.Engine.init("{\"no_such_key\":true}"));
}
