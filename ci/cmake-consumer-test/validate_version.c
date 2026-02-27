#include <stdio.h>
#include <string.h>


static int check(const char *name, const char *value, int min_len) {
    int len = (int)strlen(value);
    if (len < min_len) {
        printf("FAIL: %s = \"%s\" (len %d, expected >= %d)\n", name, value, len, min_len);
        return 1;
    }
    printf("  OK: %s = %s\n", name, value);
    return 0;
}

int main(void) {
    int failures = 0;

    printf("inject-browser-sdk CMake variable validation\n");
    printf("=============================================\n");

    /* PKGID: semver extracted from cargo pkgid, e.g. "0.1.0" */
    failures += check("INJECT_BROWSER_SDK_VERSION", INJECT_BROWSER_SDK_VERSION, 5);

    /* GIT_SHA: full 40-char hex SHA */
    failures += check("INJECT_BROWSER_SDK_GIT_SHA", INJECT_BROWSER_SDK_GIT_SHA, 40);
    if (strlen(INJECT_BROWSER_SDK_GIT_SHA) != 40) {
        printf("FAIL: GIT_SHA length is %d, expected exactly 40\n",
               (int)strlen(INJECT_BROWSER_SDK_GIT_SHA));
        failures++;
    }

    /* GIT_SHA_SHORT: 7+ char abbreviated SHA */
    failures += check("INJECT_BROWSER_SDK_GIT_SHA_SHORT", INJECT_BROWSER_SDK_GIT_SHA_SHORT, 7);

    /* VERSION: combined "semver+sha_short", e.g. "0.1.0+0b2cc63" */
    failures += check("INJECT_BROWSER_SDK_COMBINED_VERSION", INJECT_BROWSER_SDK_COMBINED_VERSION, 13);
    if (strchr(INJECT_BROWSER_SDK_COMBINED_VERSION, '+') == NULL) {
        printf("FAIL: VERSION missing '+' separator: %s\n", INJECT_BROWSER_SDK_COMBINED_VERSION);
        failures++;
    }

    printf("=============================================\n");
    if (failures) {
        printf("FAILED: %d check(s) failed\n", failures);
    } else {
        printf("ALL PASSED\n");
    }
    return failures ? 1 : 0;
}
