#include "heif.h"

#include <cstdio>
#include <string>

int main(int argc, char **argv) {
    if (argc != 3) {
        std::fputs("usage: heif-png <image> <png>\n", stderr);
        return 2;
    }
    const std::string error = decode_heif(argv[1], argv[2]);
    if (!error.empty()) {
        std::fprintf(stderr, "%s\n", error.c_str());
        return 1;
    }
    return 0;
}
