#include "firmware/common/conduit_converse.h"
#include "firmware/sat1/conduit_sat1_config.h"
#include "firmware/voicepe/conduit_voicepe_config.h"

#include <stdio.h>
#include <string.h>

static int expect_int(int actual, int expected) {
  return actual == expected ? 0 : 1;
}

static int expect_size(size_t actual, size_t expected) {
  return actual == expected ? 0 : 1;
}

static int expect_string(const char *actual, const char *expected) {
  return strcmp(actual, expected) == 0 ? 0 : 1;
}

static int parses_done_notice(void) {
  conduit_notice_t notice = conduit_notice_parse("{\"type\":\"done\"}");
  return expect_int((int) notice.type, (int) CONDUIT_NOTICE_DONE);
}

static int parses_started_notice(void) {
  conduit_notice_t notice =
      conduit_notice_parse("{\"type\":\"started\",\"conversation\":\"abc-123\"}");
  if (expect_int((int) notice.type, (int) CONDUIT_NOTICE_STARTED) != 0) {
    return 1;
  }
  if (expect_size(notice.conversation_len, 7) != 0) {
    return 1;
  }
  return strncmp(notice.conversation, "abc-123", notice.conversation_len) == 0 ? 0 : 1;
}

static int parses_failed_notice(void) {
  conduit_notice_t notice =
      conduit_notice_parse("{\"type\":\"failed\",\"error\":\"recognizer offline\"}");
  if (expect_int((int) notice.type, (int) CONDUIT_NOTICE_FAILED) != 0) {
    return 1;
  }
  if (expect_size(notice.error_len, 18) != 0) {
    return 1;
  }
  return strncmp(notice.error, "recognizer offline", notice.error_len) == 0 ? 0 : 1;
}

static int rejects_unknown_notice(void) {
  conduit_notice_t notice = conduit_notice_parse("{\"type\":\"thinking\"}");
  return expect_int((int) notice.type, (int) CONDUIT_NOTICE_UNKNOWN);
}

static int validates_pipeline_names_like_the_api(void) {
  if (!conduit_pipeline_name_is_valid("kitchen")) {
    return 1;
  }
  if (!conduit_pipeline_name_is_valid("living-room_2")) {
    return 1;
  }
  if (conduit_pipeline_name_is_valid("")) {
    return 1;
  }
  if (conduit_pipeline_name_is_valid("../kitchen")) {
    return 1;
  }
  if (conduit_pipeline_name_is_valid("kitchen light")) {
    return 1;
  }
  return 0;
}

static int builds_converse_path(void) {
  char path[80];
  size_t len = conduit_converse_path(path, sizeof(path), "kitchen");
  if (expect_string(path, "/v1/pipelines/kitchen/converse") != 0) {
    return 1;
  }
  return expect_size(len, strlen("/v1/pipelines/kitchen/converse"));
}

static int reports_required_path_length_when_truncated(void) {
  char path[12];
  size_t len = conduit_converse_path(path, sizeof(path), "kitchen");
  if (expect_string(path, "/v1/pipelin") != 0) {
    return 1;
  }
  return expect_size(len, strlen("/v1/pipelines/kitchen/converse"));
}

static int board_headers_name_their_targets(void) {
  if (expect_string(CONDUIT_SAT1_BOARD_ID, "sat1") != 0) {
    return 1;
  }
  if (expect_string(CONDUIT_VOICEPE_BOARD_ID, "voicepe") != 0) {
    return 1;
  }
  return expect_string(CONDUIT_BOARD_ID, "sat1");
}

int main(void) {
  int failures = 0;
  struct test_case {
    const char *name;
    int (*run)(void);
  } tests[] = {
      {"parses_done_notice", parses_done_notice},
      {"parses_started_notice", parses_started_notice},
      {"parses_failed_notice", parses_failed_notice},
      {"rejects_unknown_notice", rejects_unknown_notice},
      {"validates_pipeline_names_like_the_api", validates_pipeline_names_like_the_api},
      {"builds_converse_path", builds_converse_path},
      {"reports_required_path_length_when_truncated", reports_required_path_length_when_truncated},
      {"board_headers_name_their_targets", board_headers_name_their_targets},
  };

  for (size_t i = 0; i < sizeof(tests) / sizeof(tests[0]); i++) {
    if (tests[i].run() != 0) {
      fprintf(stderr, "failed: %s\n", tests[i].name);
      failures++;
    }
  }
  return failures == 0 ? 0 : 1;
}
