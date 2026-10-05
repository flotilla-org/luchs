#include "SocketRights.h"
#include <assert.h>
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <unistd.h>

static void send_rights(int socket, const int *fds, size_t count) {
    union { struct cmsghdr alignment; unsigned char bytes[CMSG_SPACE(2 * sizeof(int))]; } control;
    memset(&control, 0, sizeof(control));
    char byte = 'b';
    struct iovec iov = {&byte, 1};
    struct msghdr message = {0};
    message.msg_iov = &iov;
    message.msg_iovlen = 1;
    message.msg_control = control.bytes;
    message.msg_controllen = CMSG_SPACE(count * sizeof(int));
    struct cmsghdr *c = CMSG_FIRSTHDR(&message);
    c->cmsg_level = SOL_SOCKET;
    c->cmsg_type = SCM_RIGHTS;
    c->cmsg_len = CMSG_LEN(count * sizeof(int));
    memcpy(CMSG_DATA(c), fds, count * sizeof(int));
    assert(sendmsg(socket, &message, 0) == 1);
}

int main(void) {
    int sockets[2];
    assert(socketpair(AF_UNIX, SOCK_STREAM, 0, sockets) == 0);
    int fd = open("/dev/null", O_RDONLY);
    assert(fd >= 0);
    char byte;
    int received;

    // A descriptor attached after a plain prefix must still be received.
    assert(write(sockets[0], "a", 1) == 1);
    assert(luchs_recv_rights(sockets[1], &byte, 1, &received) == 1);
    assert(byte == 'a' && received == -1);
    send_rights(sockets[0], &fd, 1);
    assert(luchs_recv_rights(sockets[1], &byte, 1, &received) == 1);
    assert(byte == 'b' && received >= 0 && fcntl(received, F_GETFD) >= 0);
    close(received);

    // Both duplicates must be closed on rejection, without closing the sender.
    int duplicates[2] = {fd, fd};
    int next_fd = dup(fd);
    assert(next_fd >= 0);
    close(next_fd);
    send_rights(sockets[0], duplicates, 2);
    assert(luchs_recv_rights(sockets[1], &byte, 1, &received) == -1);
    assert(errno == EPROTO && received == -1);
    int after = dup(fd);
    assert(after == next_fd);
    close(after);
    // Check the second received descriptor was closed too.
    assert(fcntl(next_fd + 1, F_GETFD) == -1 && errno == EBADF);

    close(fd);
    close(sockets[0]);
    close(sockets[1]);
    puts("SocketRights tests passed");
    return 0;
}
