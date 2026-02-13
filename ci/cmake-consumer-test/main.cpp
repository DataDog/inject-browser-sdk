#include "injectbrowsersdk.h"

int main() {
    Snippet *snippet = snippet_create_from_json("{}");
    if (snippet != nullptr) {
        snippet_cleanup(snippet);
    }
    return 0;
}
