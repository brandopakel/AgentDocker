/* Temporary probe, never merged. mode 0: SO_RCVTIMEO set by the reader + blocking read (as the
 * terminal reader does); mode 2: timeout set before the reader starts;
 * mode 3: as 0 but shut down 20 ms after the reader is in read;
 * mode 4: as 0 but only the peer end is shut down; mode 1: poll with its own timeout, then
 * recv(MSG_DONTWAIT). Another fd of the same socket is shut down while the
 * reader may be entering its receive. A reader stuck 10 s aborts. */
#include <poll.h>
#include <pthread.h>
#include <sys/socket.h>
#include <sys/time.h>
#include <unistd.h>
#include <stdio.h>
#include <stdlib.h>
#include <errno.h>
#include <stdatomic.h>
static pthread_mutex_t mu = PTHREAD_MUTEX_INITIALIZER;
static int closed_flag, mode;
static atomic_int phase, started;
static volatile long it;
static void *reader(void *arg) {
  int fd = *(int *)arg;
  atomic_store(&started, 1);
  if (mode == 0 || mode == 3 || mode == 4) {
    struct timeval tv = {1, 0};
    if (setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &tv, sizeof tv) < 0) { atomic_store(&phase, 9); return 0; }
  }
  char buf[8192];
  for (;;) {
    pthread_mutex_lock(&mu); int c = closed_flag; pthread_mutex_unlock(&mu);
    if (c) { atomic_store(&phase, 8); return 0; }
    ssize_t n;
    if (mode != 1) {
      atomic_store(&phase, 2);
      n = read(fd, buf, sizeof buf);
    } else {
      struct pollfd p = {fd, POLLIN, 0};
      atomic_store(&phase, 4);
      int r = poll(&p, 1, 1000);
      if (r == 0 || (r < 0 && errno == EINTR)) continue;
      atomic_store(&phase, 5);
      n = recv(fd, buf, sizeof buf, MSG_DONTWAIT);
    }
    atomic_store(&phase, 3);
    if (n == 0) { atomic_store(&phase, 7); return 0; }
    if (n < 0 && (errno == EAGAIN || errno == EWOULDBLOCK || errno == EINTR)) continue;
    atomic_store(&phase, 6); return 0;
  }
}
static void *watchdog(void *arg) {
  long last = -1; int stuck = 0;
  for (;;) {
    sleep(1); long now = it;
    if (now == last) {
      if (++stuck == 10) {
        fprintf(stderr, "STUCK mode %d iteration %ld reader phase %d (2=in read, 4=in poll, 5=in recv)\n", mode, now, atomic_load(&phase));
        abort();
      }
    } else stuck = 0;
    last = now;
  }
}
int main(int argc, char **argv) {
  mode = atoi(argv[1]); long seconds = atol(argv[2]);
  pthread_t w; pthread_create(&w, 0, watchdog, 0);
  time_t end = time(0) + seconds; unsigned seed = 1;
  for (it = 0; time(0) < end; it++) {
    int sv[2]; if (socketpair(AF_UNIX, SOCK_STREAM, 0, sv)) { perror("socketpair"); return 1; }
    int stream = sv[0], peer = sv[1], handle = dup(stream);
    closed_flag = 0; atomic_store(&started, 0);
    if (mode == 2) { struct timeval tv = {1, 0}; setsockopt(stream, SOL_SOCKET, SO_RCVTIMEO, &tv, sizeof tv); }
    pthread_t r; pthread_create(&r, 0, reader, &stream);
    /* Land the shutdown near the reader's first receive. */
    while (!atomic_load(&started)) ;
    for (volatile int spin = rand_r(&seed) % 4000; spin > 0; spin--) ;
    if (mode == 3) {
      while (atomic_load(&phase) != 2)
        ;
      usleep(20000);
    }
    pthread_mutex_lock(&mu); closed_flag = 1; if (mode != 4) shutdown(handle, SHUT_RDWR); pthread_mutex_unlock(&mu);
    shutdown(peer, SHUT_RDWR);
    pthread_join(r, 0);
    close(handle); close(stream); close(peer);
  }
  printf("mode %d: %ld iterations, no stall\n", mode, (long)it);
  return 0;
}
