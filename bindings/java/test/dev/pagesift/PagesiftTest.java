package dev.pagesift;

import java.nio.charset.StandardCharsets;

/** 不用第三方测试框架：每项检查打印结果，有失败就非零退出。不联网。 */
public final class PagesiftTest {
    private static int passed = 0;
    private static int failed = 0;

    private static void check(boolean ok, String what) {
        if (ok) {
            passed++;
            System.out.println("  ok   " + what);
        } else {
            failed++;
            System.out.println("  FAIL " + what);
        }
    }

    public static void main(String[] args) {
        String version = Pagesift.version();
        System.out.println("libpagesift " + version);
        check(version.split("\\.").length == 3, "version looks like x.y.z");

        String md = Pagesift.convert("<h1>Hi</h1><p>World</p>", null);
        check(md.contains("\"success\":true"), "convert succeeds");
        check(md.contains("# Hi"), "convert keeps the heading");

        try (Pagesift.Engine engine = Pagesift.Engine.create(null)) {
            byte[] csv = "name,score\nalice,1\nbob,2\n".getBytes(StandardCharsets.UTF_8);
            String doc = engine.parse(csv, "scores.csv", null);
            check(doc.contains("\"success\":true"), "parse csv succeeds");
            check(doc.contains("| alice | 1 |"), "parse csv renders a table row");

            String bad = engine.parse(new byte[] {0, 1, 2, 3}, "blob.xyz", null);
            check(bad.contains("\"success\":false"), "unsupported file type reports success=false");
            check(bad.contains("\"error_code\":\"UNSUPPORTED_OPTION\""), "unsupported file type error_code");

            String badUrl = engine.scrape("not a url", null);
            check(badUrl.contains("\"error_code\":\"INVALID_REQUEST\""), "bad url error_code");

            String badOpt = engine.scrape("http://example.invalid/", "{\"no_such_option\":1}");
            check(badOpt.contains("\"error_code\":\"INVALID_REQUEST\""), "unknown option is rejected");
        }

        try {
            Pagesift.Engine.create("{\"no_such_key\":true}");
            check(false, "engine create rejects unknown config key");
        } catch (IllegalArgumentException e) {
            check(!e.getMessage().isEmpty(), "engine create rejects unknown config key");
        }

        System.out.println("\n" + passed + " passed, " + failed + " failed");
        System.exit(failed == 0 ? 0 : 1);
    }
}
