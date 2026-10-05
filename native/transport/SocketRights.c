#include "SocketRights.h"
#include <sys/socket.h>
#include <unistd.h>
#include <errno.h>
#include <string.h>

ssize_t luchs_recv_rights(int socket, void *bytes, size_t length, int *descriptor) {
    union { struct cmsghdr alignment; unsigned char bytes[CMSG_SPACE(8 * sizeof(int))]; } control;
    struct iovec iov = {bytes, length};
    struct msghdr message = {0};
    message.msg_iov = &iov;
    message.msg_iovlen = 1;
    message.msg_control = control.bytes;
    message.msg_controllen = sizeof(control.bytes);
    *descriptor = -1;
    ssize_t count = recvmsg(socket, &message, 0);
    if (count < 0) return count;
    int invalid = (message.msg_flags & MSG_CTRUNC) != 0;
    for (struct cmsghdr *c = CMSG_FIRSTHDR(&message); c; c = CMSG_NXTHDR(&message, c)) {
        if (c->cmsg_level != SOL_SOCKET || c->cmsg_type != SCM_RIGHTS || c->cmsg_len < CMSG_LEN(0)) {
            invalid = 1;
            continue;
        }
        size_t n = (c->cmsg_len - CMSG_LEN(0)) / sizeof(int);
        for (size_t i = 0; i < n; i++) {
            int fd;
            memcpy(&fd, CMSG_DATA(c) + i * sizeof(int), sizeof(fd));
            if (*descriptor != -1) { close(fd); invalid = 1; }
            else *descriptor = fd;
        }
        if (n != 1) invalid = 1;
    }
    if (invalid) {
        if (*descriptor != -1) close(*descriptor);
        *descriptor = -1;
        errno = EPROTO;
        return -1;
    }
    return count;
}
