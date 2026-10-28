#include <errno.h>
#include <stdio.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

int main(void) {
    pid_t inherited_group = getpgrp();
    pid_t inherited_session = getsid(0);
    pid_t child = fork();
    if (child < 0) { perror("fork"); return 1; }
    if (child == 0) {
        fprintf(stderr, "child pid=%d pgrp=%d session=%d parent-group=%d parent-session=%d\n", getpid(), getpgrp(), getsid(0), inherited_group, inherited_session);
        if (setpgid(0, inherited_group) < 0) perror("setpgid");
        if (getsid(0) != inherited_session) fprintf(stderr, "session mismatch\n");
        if (setsid() < 0) { perror("setsid"); _exit(errno); }
        fprintf(stderr, "setsid success\n");
        _exit(0);
    }
    int status;
    waitpid(child, &status, 0);
    return WIFEXITED(status) ? WEXITSTATUS(status) : 2;
}
