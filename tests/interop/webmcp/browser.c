#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

int main(int argc, char **argv)
{
    char **args = calloc((size_t)argc + 2, sizeof(*args));
    if (!args)
        return 127;
    const char prefix[] = "--enable-features=";
    const char *existing = "";
    for (int i = 1; i < argc; i++)
        if (!strncmp(argv[i], prefix, sizeof(prefix) - 1))
            existing = argv[i] + sizeof(prefix) - 1;
    const char features[] = "WebMCPTesting,DevToolsWebMCPSupport";
    size_t length = sizeof(prefix) + strlen(existing) + sizeof(features) + 1;
    char *flag = malloc(length);
    if (!flag) {
        free(args);
        return 127;
    }
    snprintf(flag, length, "%s%s%s%s", prefix, existing, *existing ? "," : "", features);
    args[0] = "/opt/browser/helium.real";
    for (int i = 1; i < argc; i++)
        args[i] = argv[i];
    args[argc] = flag;
    execv(args[0], args);
    perror("exec helium");
    free(flag);
    free(args);
    return 127;
}
