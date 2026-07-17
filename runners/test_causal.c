#define _POSIX_C_SOURCE 200809L
#define REPROIT_CAUSAL_IMPLEMENTATION
#include "reproit_causal.h"
#include <assert.h>
#include <sys/stat.h>
#include <unistd.h>

static int live(void *user, const char *method, const char *url, const char *qh, const char *qb,
                char *rh, size_t rhcap, char *rb, size_t rbcap) {
  (void)user;
  (void)method;
  (void)url;
  (void)qh;
  (void)qb;
  snprintf(rh, rhcap, "{\"content-type\":\"application/json\",\"set-cookie\":\"raw\"}");
  snprintf(rb, rbcap, "{\"ok\":true,\"email\":\"a@b.c\"}");
  return 201;
}
int main(void) {
  char dir[256], network[300], action[300], caps[300], capsule[300];
  snprintf(dir, sizeof dir, "/tmp/reproit-causal-%d", (int)getpid());
  mkdir(dir, 0700);
  snprintf(network, sizeof network, "%s/network.jsonl", dir);
  snprintf(action, sizeof action, "%s/action", dir);
  snprintf(caps, sizeof caps, "%s/caps.json", dir);
  snprintf(capsule, sizeof capsule, "%s/capsule.json", dir);
  FILE *f = fopen(action, "wb");
  fputs("1", f);
  fclose(f);
  setenv("REPROIT_NETWORK_FILE", network, 1);
  setenv("REPROIT_ACTION_FILE", action, 1);
  setenv("REPROIT_CAPABILITIES_FILE", caps, 1);
  setenv("REPROIT_DEVICE", "a", 1);
  assert(ReproIt_Causal_Enable());
  char rh[1024], rb[1024];
  assert(ReproIt_Causal_Json(
             "POST", "https://api.test/x", "{\"authorization\":\"raw\"}",
             "{\"token\":\"raw\",\"apiKey\":\"raw-api\",\"publishable-key\":\"raw-pub\",\"private_"
             "key\":\"raw-private\",\"access.key\":\"raw-access\",\"signing "
             "key\":\"raw-signing\",\"keyboardLayout\":\"dvorak\",\"key\":\"ordinary\",\"kind\":"
             "\"x\"}",
             rh, sizeof rh, rb, sizeof rb, live, NULL) == 201);
  assert(strstr(rb, "a@b.c")); /* capture redaction must not mutate the live app response */
  char *line = reproit_causal_read(network);
  assert(line && strstr(line, "<reproit:string:length=3>") && !strstr(line, "a@b.c"));
  assert(strstr(line, "keyboardLayout") && strstr(line, "dvorak") && strstr(line, "ordinary"));
  assert(!strstr(line, "raw-api") && !strstr(line, "raw-pub") && !strstr(line, "raw-private") &&
         !strstr(line, "raw-access") && !strstr(line, "raw-signing"));
  free(line);
  f = fopen(capsule, "wb");
  fputs("{\"exchanges\":[{\"id\":\"a-1-0\",\"actor\":\"a\",\"actionIndex\":1,\"ordinal\":0,"
        "\"method\":\"GET\",\"url\":\"https://api.test/"
        "config?a=1&b=2\",\"status\":200,\"responseHeaders\":{\"content-type\":\"application/"
        "json\"},\"responseBody\":{\"enabled\":true},\"required\":true}]}",
        f);
  fclose(f);
  free(reproit_causal_capsule);
  reproit_causal_capsule = reproit_causal_read(capsule);
  reproit_causal_prior_action = ~0u;
  reproit_causal_ordinal = 0;
  assert(ReproIt_Causal_Json("GET", "https://api.test/config?b=2&a=1", "{}", "null", rh, sizeof rh,
                             rb, sizeof rb, NULL, NULL) == 200);
  assert(strstr(rb, "enabled"));
  assert(ReproIt_Causal_Json("GET", "https://api.test/miss", "{}", "null", rh, sizeof rh, rb,
                             sizeof rb, NULL, NULL) == -1);
  remove(network);
  remove(action);
  remove(caps);
  remove(capsule);
  rmdir(dir);
  return 0;
}
