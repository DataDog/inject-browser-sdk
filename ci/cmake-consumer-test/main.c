#include "injectbrowsersdk.h"

int main(void) {
    struct Snippet *snippet = snippet_create_from_json("{}");
    if (snippet != 0) {
        snippet_cleanup(snippet);
    }
    return 0;
}
