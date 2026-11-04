// Swift 绑定测试：每项检查打印结果，有失败就非零退出。不联网，不用 XCTest。
import Foundation

var passed = 0, failed = 0
func check(_ ok: Bool, _ what: String) {
    if ok { passed += 1; print("  ok   \(what)") } else { failed += 1; print("  FAIL \(what)") }
}

let v = Pagesift.version
print("libpagesift \(v)")
check(v.split(separator: ".").count == 3, "version looks like x.y.z")

let md = Pagesift.convert("<h1>Hi</h1><p>World</p>")
check(md.contains("\"success\":true"), "convert succeeds")
check(md.contains("# Hi"), "convert keeps the heading")

do {
    let e = try Pagesift.Engine()
    let doc = try e.parse(Array("name,score\nalice,1\nbob,2\n".utf8), filename: "scores.csv")
    check(doc.contains("\"success\":true"), "parse csv succeeds")
    check(doc.contains("| alice | 1 |"), "parse csv renders a table row")
    let bad = try e.parse([0, 1, 2, 3], filename: "blob.xyz")
    check(bad.contains("\"success\":false"), "unsupported file type reports success=false")
    check(bad.contains("\"error_code\":\"UNSUPPORTED_OPTION\""), "unsupported file type error_code")
    check(try e.scrape("not a url").contains("\"error_code\":\"INVALID_REQUEST\""), "bad url error_code")
    e.close()
    check((try? e.scrape("http://example.invalid/")) == nil, "closed engine raises")
} catch {
    check(false, "engine test threw: \(error)")
}

do {
    _ = try Pagesift.Engine(config: "{\"no_such_key\":true}")
    check(false, "engine create rejects unknown config key")
} catch {
    check(!"\(error)".isEmpty, "engine create rejects unknown config key")
}

print("\n\(passed) passed, \(failed) failed")
exit(failed == 0 ? 0 : 1)
