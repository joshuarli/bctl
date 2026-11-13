#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/wait.h>
#include <unistd.h>

int main(int argc, char **argv)
{
    if (argc > 1)
        return argc == 3 && !strcmp(argv[1], "after") && !strcmp(argv[2], "a b") &&
            getenv("EXEC_MARKER") && !strcmp(getenv("EXEC_MARKER"), "yes") ? 42 : 1;

    pid_t child = fork();
    if (child == 0) {
        char *args[] = {argv[0], "after", "a b", 0};
        char *env[] = {"EXEC_MARKER=yes", 0};
        execve(argv[0], args, env);
        _exit(127);
    }
    int status = 0;
    int ok = child > 0 && waitpid(child, &status, 0) == child &&
        WIFEXITED(status) && WEXITSTATUS(status) == 42;
    printf("MINI_CHROMIUM_TEST name=exec_self status=%s\n", ok ? "PASS" : "FAIL");
    printf("MINI_CHROMIUM_RESULT status=%s passed=%d failed=%d\n", ok ? "PASS" : "FAIL", ok, !ok);
    return !ok;
}
