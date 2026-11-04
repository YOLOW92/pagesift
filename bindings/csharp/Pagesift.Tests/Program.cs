// 不用 xunit 等第三方包：每项检查打印结果，有失败就非零退出。不联网。
using System.Text;
using PagesiftLib;

int passed = 0, failed = 0;

void Check(bool ok, string what)
{
    if (ok) { passed++; Console.WriteLine("  ok   " + what); }
    else { failed++; Console.WriteLine("  FAIL " + what); }
}

var version = Pagesift.Version();
Console.WriteLine("libpagesift " + version);
Check(version.Split('.').Length == 3, "version looks like x.y.z");

var md = Pagesift.Convert("<h1>Hi</h1><p>World</p>");
Check(md.Contains("\"success\":true"), "convert succeeds");
Check(md.Contains("# Hi"), "convert keeps the heading");

using (var engine = Pagesift.Engine.Create())
{
    var csv = Encoding.UTF8.GetBytes("name,score\nalice,1\nbob,2\n");
    var doc = engine.Parse(csv, "scores.csv");
    Check(doc.Contains("\"success\":true"), "parse csv succeeds");
    Check(doc.Contains("| alice | 1 |"), "parse csv renders a table row");

    var bad = engine.Parse(new byte[] { 0, 1, 2, 3 }, "blob.xyz");
    Check(bad.Contains("\"success\":false"), "unsupported file type reports success=false");
    Check(bad.Contains("\"error_code\":\"UNSUPPORTED_OPTION\""), "unsupported file type error_code");

    var badUrl = engine.Scrape("not a url");
    Check(badUrl.Contains("\"error_code\":\"INVALID_REQUEST\""), "bad url error_code");

    var badOpt = engine.Scrape("http://example.invalid/", "{\"no_such_option\":1}");
    Check(badOpt.Contains("\"error_code\":\"INVALID_REQUEST\""), "unknown option is rejected");
}

try
{
    Pagesift.Engine.Create("{\"no_such_key\":true}").Dispose();
    Check(false, "engine create rejects unknown config key");
}
catch (ArgumentException e)
{
    Check(e.Message.Length > 0, "engine create rejects unknown config key");
}

Console.WriteLine($"\n{passed} passed, {failed} failed");
return failed == 0 ? 0 : 1;
