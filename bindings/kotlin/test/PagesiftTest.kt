import dev.pagesift.Pagesift

/** Kotlin 直接调用 Java 封装（dev.pagesift.Pagesift）。不用第三方测试框架，不联网。 */
var passed = 0
var failed = 0

fun check(ok: Boolean, what: String) {
    if (ok) { passed++; println("  ok   $what") } else { failed++; println("  FAIL $what") }
}

fun main() {
    val version = Pagesift.version()
    println("libpagesift $version")
    check(version.split(".").size == 3, "version looks like x.y.z")

    val md = Pagesift.convert("<h1>Hi</h1><p>World</p>", null)
    check(md.contains("\"success\":true"), "convert succeeds")
    check(md.contains("# Hi"), "convert keeps the heading")

    Pagesift.Engine.create(null).use { engine ->
        val doc = engine.parse("name,score\nalice,1\nbob,2\n".toByteArray(), "scores.csv", null)
        check(doc.contains("\"success\":true"), "parse csv succeeds")
        check(doc.contains("| alice | 1 |"), "parse csv renders a table row")

        val bad = engine.parse(byteArrayOf(0, 1, 2, 3), "blob.xyz", null)
        check(bad.contains("\"success\":false"), "unsupported file type reports success=false")
        check(bad.contains("\"error_code\":\"UNSUPPORTED_OPTION\""), "unsupported file type error_code")

        val badUrl = engine.scrape("not a url", null)
        check(badUrl.contains("\"error_code\":\"INVALID_REQUEST\""), "bad url error_code")
    }

    try {
        Pagesift.Engine.create("{\"no_such_key\":true}").close()
        check(false, "engine create rejects unknown config key")
    } catch (e: IllegalArgumentException) {
        check(!e.message.isNullOrEmpty(), "engine create rejects unknown config key")
    }

    println("\n$passed passed, $failed failed")
    if (failed > 0) System.exit(1)
}
