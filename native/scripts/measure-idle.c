// Read-only macOS sampler: measure-idle <Telecat PID> [seconds, default 20].
// Launch using scenario.sh first (it supplies a temp profile and no Keychain).
#include <errno.h>
#include <libproc.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/resource.h>
#include <time.h>
#include <unistd.h>
static double now(void) {
    struct timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    return t.tv_sec + t.tv_nsec / 1e9;
}
int main(int argc, char **argv) {
    char *end;
    if (argc < 2 || argc > 3) {
        fprintf(stderr, "usage: measure-idle <PID> [seconds]\n"); return 2;
    }
    long pid = strtol(argv[1], &end, 10);
    if (*end || pid < 1 || pid > 2147483647) return 2;
    long seconds = argc == 3 ? strtol(argv[2], &end, 10) : 20;
    if (*end || seconds < 1 || seconds > 60) return 2;
    struct rusage_info_v2 a = {0}, b = {0};
    if (proc_pid_rusage((int)pid, RUSAGE_INFO_V2, (rusage_info_t *)&a)) { perror("proc_pid_rusage"); return 1; }
    double start = now();
    struct timespec delay = {.tv_sec = seconds};
    while (nanosleep(&delay, &delay) && errno == EINTR) {}
    if (proc_pid_rusage((int)pid, RUSAGE_INFO_V2, (rusage_info_t *)&b)) { perror("proc_pid_rusage"); return 1; }
    double elapsed = now() - start;
    printf("PID %ld: %.2f s; CPU %.3f%%; interrupt wakeups %.3f/s; package idle wakeups %.3f/s; footprint %.2f MiB\n", pid, elapsed,
        ((b.ri_user_time-a.ri_user_time)+(b.ri_system_time-a.ri_system_time))/1e9/elapsed*100.,
        (b.ri_interrupt_wkups-a.ri_interrupt_wkups)/elapsed,
        (b.ri_pkg_idle_wkups-a.ri_pkg_idle_wkups)/elapsed,
        b.ri_phys_footprint/1048576.);
    return 0;
}
