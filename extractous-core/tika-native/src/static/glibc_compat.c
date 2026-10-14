#define _GNU_SOURCE
#include <dirent.h>
#include <fcntl.h>
#include <math.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/resource.h>
#include <sys/sendfile.h>
#include <sys/stat.h>
#include <sys/statvfs.h>
#include <unistd.h>

FILE *fopen64(const char *path, const char *mode) {
    return fopen(path, mode);
}

int open64(const char *path, int flags, ...) {
    mode_t mode = 0;
    if ((flags & O_CREAT) != 0 || (flags & O_TMPFILE) == O_TMPFILE) {
        va_list arguments;
        va_start(arguments, flags);
        mode = va_arg(arguments, mode_t);
        va_end(arguments);
    }
    return open(path, flags, mode);
}

off_t lseek64(int fd, off_t offset, int whence) {
    return lseek(fd, offset, whence);
}

ssize_t pread64(int fd, void *buffer, size_t count, off_t offset) {
    return pread(fd, buffer, count, offset);
}

ssize_t pwrite64(int fd, const void *buffer, size_t count, off_t offset) {
    return pwrite(fd, buffer, count, offset);
}

int ftruncate64(int fd, off_t length) {
    return ftruncate(fd, length);
}

int posix_fadvise64(int fd, off_t offset, off_t length, int advice) {
    return posix_fadvise(fd, offset, length, advice);
}

void *mmap64(void *address, size_t length, int protection, int flags, int fd, off_t offset) {
    return mmap(address, length, protection, flags, fd, offset);
}

ssize_t sendfile64(int out, int in, off_t *offset, size_t count) {
    return sendfile(out, in, offset, count);
}

struct dirent *readdir64(DIR *directory) {
    return readdir(directory);
}

int statvfs64(const char *path, struct statvfs *buffer) {
    return statvfs(path, buffer);
}

int fstatvfs64(int fd, struct statvfs *buffer) {
    return fstatvfs(fd, buffer);
}

int getrlimit64(int resource, struct rlimit *limit) {
    return getrlimit(resource, limit);
}

int __xstat64(int version, const char *path, struct stat *buffer) {
    (void) version;
    return stat(path, buffer);
}

int __lxstat64(int version, const char *path, struct stat *buffer) {
    (void) version;
    return lstat(path, buffer);
}

int __fxstat64(int version, int fd, struct stat *buffer) {
    (void) version;
    return fstat(fd, buffer);
}

int __isinf(double value) {
    return isinf(value) ? (signbit(value) ? -1 : 1) : 0;
}

int __isnan(double value) {
    return isnan(value);
}

int __isnanf(float value) {
    return isnan(value);
}

char *__strdup(const char *text) {
    return strdup(text);
}

char *__strtok_r(char *text, const char *delimiters, char **saved) {
    return strtok_r(text, delimiters, saved);
}
