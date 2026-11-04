-- LuaJIT 绑定测试：每项检查打印结果，有失败就非零退出。不联网。
package.path = debug.getinfo(1, "S").source:match("^@(.*)/test/[^/]*$") .. "/?.lua;" .. package.path
local pagesift = require("pagesift")

local passed, failed = 0, 0
local function check(ok, what)
  if ok then passed = passed + 1; print("  ok   " .. what)
  else failed = failed + 1; print("  FAIL " .. what) end
end
local function has(s, sub) return s ~= nil and s:find(sub, 1, true) ~= nil end

local v = pagesift.version()
print("libpagesift " .. v)
check(v:match("^%d+%.%d+%.%d+") ~= nil, "version looks like x.y.z")

local md = pagesift.convert("<h1>Hi</h1><p>World</p>")
check(has(md, '"success":true'), "convert succeeds")
check(has(md, "# Hi"), "convert keeps the heading")

local e = pagesift.engine()
local doc = e:parse("name,score\nalice,1\nbob,2\n", "scores.csv")
check(has(doc, '"success":true'), "parse csv succeeds")
check(has(doc, "| alice | 1 |"), "parse csv renders a table row")
local bad = e:parse("\0\1\2\3", "blob.xyz")
check(has(bad, '"success":false'), "unsupported file type reports success=false")
check(has(bad, '"error_code":"UNSUPPORTED_OPTION"'), "unsupported file type error_code")
check(has(e:scrape("not a url"), '"error_code":"INVALID_REQUEST"'), "bad url error_code")
e:close()
check(not pcall(function() e:scrape("http://example.invalid/") end), "closed engine raises")

local ok, err = pcall(pagesift.engine, '{"no_such_key":true}')
check(not ok and #tostring(err) > 0, "engine create rejects unknown config key")

print(("\n%d passed, %d failed"):format(passed, failed))
os.exit(failed == 0 and 0 or 1)
