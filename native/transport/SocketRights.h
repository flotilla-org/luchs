#pragma once
#include <sys/types.h>
#include <stddef.h>
// Receive bounded bytes and at most one SCM_RIGHTS descriptor. On error all
// descriptors in this message are closed. Caller owns a returned descriptor.
ssize_t luchs_recv_rights(int socket, void *bytes, size_t length, int *descriptor);
