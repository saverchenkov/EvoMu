#include <stdio.h>

extern int Py_NoSiteFlag;
extern void Py_Initialize(void);
extern int PyRun_SimpleFileExFlags(FILE *fp, const char *filename, int closeit, void *flags);
extern void PySys_SetArgv(int argc, char **argv);

int main(int argc, char **argv) {
    if (argc < 2) {
        fprintf(stderr, "Usage: %s <script.py> [args...]\n", argv[0]);
        return 2;
    }
    FILE *fp = fopen(argv[1], "r");
    if (!fp) {
        perror("fopen");
        return 2;
    }
    Py_NoSiteFlag = 1;
    Py_Initialize();
    PySys_SetArgv(argc - 1, argv + 1);
    return PyRun_SimpleFileExFlags(fp, argv[1], 1, NULL);
}
