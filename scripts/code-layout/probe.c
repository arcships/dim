#define _GNU_SOURCE
#include <dlfcn.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

static int (*entry)(int, char **, char **);

static int trace_main(int argc, char **argv, char **envp) {
    char exe[4096], line[8192], path[4096], permissions[8];
    unsigned long start, end;
    unsigned long code_start = 0, code_end = 0;
    ssize_t length = readlink("/proc/self/exe", exe, sizeof(exe) - 1);
    if (length < 0 || length == sizeof(exe) - 1) _exit(121);
    exe[length] = 0;
    if (sysconf(_SC_PAGESIZE) != 4096) _exit(122);
    FILE *maps = fopen("/proc/self/maps", "r");
    if (!maps) _exit(123);
    while (fgets(line, sizeof(line), maps)) {
        path[0] = 0;
        if (sscanf(line, "%lx-%lx %7s %*s %*s %*s %4095[^\n]",
                   &start, &end, permissions, path) != 4 ||
            strcmp(path, exe) || strcmp(permissions, "r-xp")) continue;
        code_start = start;
        code_end = end;
        for (unsigned long at = start; at < end; at += 4096) {
            (void)*(volatile unsigned char *)at;
            if ((at - start) % 8192 &&
                madvise((void *)at, 4096, MADV_DONTDUMP)) _exit(124);
        }
        break;
    }
    fclose(maps);
    int stop_after = getenv("DIM_LAYOUT_NO_EXIT_STOP") == NULL;
    unsetenv("DIM_LAYOUT_NO_EXIT_STOP");
    unsetenv("LD_PRELOAD");
    raise(SIGSTOP);
    if (!code_start || mprotect((void *)code_start, code_end - code_start, PROT_NONE) ||
        mprotect((void *)code_start, code_end - code_start, PROT_READ | PROT_EXEC)) _exit(126);
    int result = entry(argc, argv, envp);
    if (stop_after) raise(SIGSTOP);
    return result;
}

int __libc_start_main(int (*main_fn)(int, char **, char **), int argc, char **argv,
                      void (*init)(void), void (*fini)(void),
                      void (*rtld_fini)(void), void *stack_end) {
    int (*next)(int (*)(int, char **, char **), int, char **,
                void (*)(void), void (*)(void), void (*)(void), void *) =
        dlsym(RTLD_NEXT, "__libc_start_main");
    if (!next) _exit(125);
    entry = main_fn;
    return next(trace_main, argc, argv, init, fini, rtld_fini, stack_end);
}
