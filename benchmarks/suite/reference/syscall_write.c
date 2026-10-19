/* The C twin of cases/micro/syscall_write.nx.
 *
 * The same three operations the Nexum case does - open, write one byte,
 * close - through the C library, which is what the runtime does on
 * Windows. On POSIX the runtime reaches the kernel through its own syscall
 * layer and the equivalent twin would use open(2), write(2) and close(2)
 * directly.
 *
 * The case itself is recorded as not yet measurable: the runtime's write
 * wrapper is not exposed to a program, so a bare write(2) cannot be
 * measured yet, and three syscalls are not the syscall overhead claim.
 * The twin is here so the comparison is ready the day the builtin lands.
 *
 *   usage: syscall_write <case|base> <ops>
 */

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

int main(int argc, char **argv)
{
    long ops = argc > 2 ? atol(argv[2]) : 1000;

    for (long i = 0; i < ops; i++) {
        FILE *f = fopen("nx_bench_syscall.tmp", "wb");
        if (f == NULL) {
            return 1;
        }
        fwrite("x", 1, 1, f);
        fclose(f);
    }

    printf("%ld\n", ops);
    return 0;
}
