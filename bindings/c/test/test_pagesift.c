/* libpagesift C ABI 测试：不连外网，只跑本地 HTML 转换、CSV 解析和错误路径。 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "pagesift.h"

static int passed = 0, failed = 0;

#define CHECK(cond, what)                                               \
    do {                                                                \
        if (cond) { passed++; printf("  ok   %s\n", what); }            \
        else { failed++; printf("  FAIL %s\n", what); }                \
    } while (0)

/* 结果是 JSON，字段都是 "key":value 的紧凑形式（serde_json 输出）。这里只做子串判断。 */
static int has(const char *json, const char *needle) {
    return json != NULL && strstr(json, needle) != NULL;
}

int main(void) {
    printf("libpagesift %s\n", pagesift_version());

    /* 版本号：非空，形如 x.y.z */
    const char *v = pagesift_version();
    CHECK(v != NULL && strlen(v) >= 5 && strchr(v, '.') != NULL, "version looks like x.y.z");

    /* convert：HTML 转 Markdown，不需要引擎 */
    char *r = pagesift_convert("<h1>Hi</h1><p>World</p>", NULL);
    CHECK(has(r, "\"success\":true"), "convert succeeds");
    CHECK(has(r, "# Hi"), "convert keeps the heading");
    pagesift_string_free(r);

    /* 建引擎；不给配置、不联网 */
    char *err = NULL;
    pagesift_engine *e = pagesift_engine_new(NULL, &err);
    CHECK(e != NULL && err == NULL, "engine_new with NULL config");

    /* parse：字节输入的 CSV */
    const char csv[] = "name,score\nalice,1\nbob,2\n";
    r = pagesift_parse(e, (const uint8_t *)csv, strlen(csv), "scores.csv", NULL);
    CHECK(has(r, "\"success\":true"), "parse csv succeeds");
    CHECK(has(r, "| alice | 1 |"), "parse csv renders a table row");
    pagesift_string_free(r);

    /* 错误路径 1：不支持的文件类型 */
    const unsigned char junk[] = {0x00, 0x01, 0x02, 0x03};
    r = pagesift_parse(e, junk, sizeof junk, "blob.xyz", NULL);
    CHECK(has(r, "\"success\":false"), "unsupported file type reports success=false");
    CHECK(has(r, "\"error_code\":\"UNSUPPORTED_OPTION\""), "unsupported file type error_code");
    pagesift_string_free(r);

    /* 错误路径 2：坏 URL */
    r = pagesift_scrape(e, "not a url", NULL);
    CHECK(has(r, "\"error_code\":\"INVALID_REQUEST\""), "bad url error_code");
    pagesift_string_free(r);

    /* 错误路径 3：未知选项必须报错，不能悄悄忽略 */
    r = pagesift_scrape(e, "http://example.invalid/", "{\"no_such_option\":1}");
    CHECK(has(r, "\"error_code\":\"INVALID_REQUEST\""), "unknown option is rejected");
    pagesift_string_free(r);

    /* 错误路径 4：引擎为 NULL */
    r = pagesift_parse(NULL, (const uint8_t *)csv, strlen(csv), "scores.csv", NULL);
    CHECK(has(r, "\"error_code\":\"INVALID_REQUEST\""), "NULL engine is rejected");
    pagesift_string_free(r);

    pagesift_engine_free(e);
    pagesift_string_free(NULL); /* NULL 是无操作 */

    /* 错误配置：engine_new 失败要给出错误串，返回 NULL */
    char *cfg_err = NULL;
    pagesift_engine *bad = pagesift_engine_new("{\"no_such_key\":true}", &cfg_err);
    CHECK(bad == NULL && cfg_err != NULL, "engine_new rejects unknown config key");
    pagesift_string_free(cfg_err);

    printf("\n%d passed, %d failed\n", passed, failed);
    return failed == 0 ? 0 : 1;
}
