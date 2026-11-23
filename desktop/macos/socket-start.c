// Runs inside Terminal.app. Only SSH diagnostics and the exit status go back
// to the sheet. OpenSSH reads passwords directly from its controlling terminal.
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <termios.h>
#include <unistd.h>
#ifdef __APPLE__
#include <util.h>
#else
#include <pty.h>
#endif

static void write_bytes(int fd, const void *data, size_t count) {
    const char *p = data;
    while (count) {
        ssize_t n = write(fd, p, count);
        if (n < 0 && errno == EINTR) continue;
        if (n <= 0) break;
        p += n;
        count -= (size_t)n;
    }
}

static void publish(int dir, const char *name, int number, const char *detail, size_t size) {
    if (dir == -1) return;
    int fd = openat(dir, "pending", O_WRONLY | O_CREAT | O_EXCL | O_NOFOLLOW, 0600);
    if (fd == -1) return;  // The sheet may have closed and removed the directory.
    char header[32];
    int len = snprintf(header, sizeof header, "%d\n", number);
    write_bytes(fd, header, (size_t)len);
    write_bytes(fd, detail, size);
    close(fd);
    renameat(dir, "pending", dir, name);
}

static int system_error(int dir, const char *operation) {
    char message[512];
    snprintf(message, sizeof message, "%s: %s", operation, strerror(errno));
    fprintf(stderr, "%s\n", message);
    publish(dir, "result", 1, message, strlen(message));
    return 1;
}

int main(int argc, char **argv) {
    if (argc != 4) return 2;
    char *const command[] = {argv[2], "socket", "start", "--", argv[3], NULL};
    int dir = open(argv[1], O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC);
    struct stat meta;
    if (dir != -1 && (fstat(dir, &meta) || meta.st_uid != geteuid() || (meta.st_mode & 0077))) {
        close(dir);
        dir = -1;
    }
    publish(dir, "started", getpid(), NULL, 0);
    int reader, writer;
    if (openpty(&reader, &writer, NULL, NULL, NULL)) return system_error(dir, "openpty");
    struct termios mode;
    if (tcgetattr(writer, &mode) == 0) {
        mode.c_oflag &= ~OPOST;
        tcsetattr(writer, TCSANOW, &mode);
    }
    pid_t child = fork();
    if (child == -1) return system_error(dir, "fork");
    if (child == 0) {
        close(reader);
        if (dup2(writer, STDERR_FILENO) == -1) _exit(1);
        close(writer);
        execv(command[0], command);
        perror("sshi");
        _exit(127);
    }
    close(writer);
    // Ctrl-C still reaches SSH in the foreground process group.
    signal(SIGINT, SIG_IGN);
    signal(SIGQUIT, SIG_IGN);
    char tail[4096];
    size_t used = 0;
    int status;
    pid_t exited = 0;
    struct pollfd stream = {.fd = reader, .events = POLLIN};
    while (1) {
        if (!exited) {
            exited = waitpid(child, &status, WNOHANG);
            if (exited < 0) {
                if (errno == EINTR) { exited = 0; continue; }
                return system_error(dir, "waitpid");
            }
        }
        int ready = poll(&stream, 1, exited ? 0 : 100);
        if (ready < 0 && errno == EINTR) continue;
        if (!ready && !exited) continue;
        if (ready <= 0) break;
        char chunk[1024];
        ssize_t count = read(reader, chunk, sizeof chunk);
        if (count < 0 && errno == EINTR) continue;
        if (count <= 0) break;
        write_bytes(STDERR_FILENO, chunk, (size_t)count);
        size_t keep = used < sizeof tail - (size_t)count ? used : sizeof tail - (size_t)count;
        memmove(tail, tail + used - keep, keep);
        memcpy(tail + keep, chunk, (size_t)count);
        used = keep + (size_t)count;
    }
    close(reader);
    while (!exited) {
        exited = waitpid(child, &status, 0);
        if (exited < 0) {
            if (errno != EINTR) return system_error(dir, "waitpid");
            exited = 0;
        }
    }
    int code = WIFEXITED(status) ? WEXITSTATUS(status) : 128 + WTERMSIG(status);
    publish(dir, "result", code, tail, code ? used : 0);
    if (dir != -1) close(dir);
    return code;
}
