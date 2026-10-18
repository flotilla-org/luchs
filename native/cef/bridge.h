// Private Luchs engine seam, not a plugin ABI.
#pragma once
#include <stddef.h>
#include <stdint.h>
struct LuchsCefConfig {
  const char *url;
  const char *cache;
  uint32_t width, height, fps;
  int gpu;
};
struct LuchsCefCallbacks {
  void (*pixels)(void *, const uint8_t *, uint32_t, uint32_t, uint64_t);
  void (*surface)(void *, void *, uint32_t, uint32_t, uint64_t);
  void (*state)(void *, const char *);
  int (*tick)(void *);
  int (*allowed)(void *, const char *);
};
#ifdef __cplusplus
extern "C" {
#endif
int luchs_cef_run(const struct LuchsCefConfig *, struct LuchsCefCallbacks,
                  void *);
int luchs_cef_command(const char *, uint64_t);
#ifdef __cplusplus
}
#endif
