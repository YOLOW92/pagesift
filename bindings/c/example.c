/* 最小示例：把一段 HTML 转成 Markdown，再解析一个 CSV。编译方法见 README.md。 */
#include <stdio.h>
#include "pagesift.h"

int main(void) {
    char *md = pagesift_convert("<h1>Hello</h1><p>from C</p>", NULL);
    printf("%s\n", md); /* 输出的是 Document JSON，字段 markdown 里有正文 */
    pagesift_string_free(md);

    char *err = NULL;
    pagesift_engine *e = pagesift_engine_new(NULL, &err);
    if (!e) {
        fprintf(stderr, "engine: %s\n", err);
        pagesift_string_free(err);
        return 1;
    }
    const char csv[] = "a,b\n1,2\n";
    char *doc = pagesift_parse(e, (const unsigned char *)csv, sizeof csv - 1, "t.csv", NULL);
    printf("%s\n", doc);
    pagesift_string_free(doc);
    pagesift_engine_free(e);
    return 0;
}
