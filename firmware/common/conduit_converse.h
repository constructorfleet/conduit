#ifndef CONDUIT_CONVERSE_H
#define CONDUIT_CONVERSE_H

#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

#define CONDUIT_AUDIO_SAMPLE_RATE_HZ 16000
#define CONDUIT_AUDIO_CHANNELS 1
#define CONDUIT_AUDIO_BITS_PER_SAMPLE 16
#define CONDUIT_CONVERSE_END_JSON "{\"type\":\"end\"}"
#define CONDUIT_CONVERSE_PATH_PREFIX "/v1/pipelines/"
#define CONDUIT_CONVERSE_PATH_SUFFIX "/converse"

typedef enum conduit_notice_type {
  CONDUIT_NOTICE_UNKNOWN = 0,
  CONDUIT_NOTICE_STARTED,
  CONDUIT_NOTICE_DONE,
  CONDUIT_NOTICE_FAILED,
} conduit_notice_type_t;

typedef struct conduit_notice {
  conduit_notice_type_t type;
  const char *conversation;
  size_t conversation_len;
  const char *error;
  size_t error_len;
} conduit_notice_t;

static inline int conduit_streq_literal(const char *value, size_t len, const char *literal) {
  size_t i = 0;
  while (literal[i] != '\0') {
    if (i >= len || value[i] != literal[i]) {
      return 0;
    }
    i++;
  }
  return i == len;
}

static inline const char *conduit_json_string_value(
    const char *json,
    const char *key,
    size_t *value_len) {
  const char *cursor = json;
  while (*cursor != '\0') {
    const char *candidate = cursor;
    const char *needle = key;
    while (*candidate == *needle && *needle != '\0') {
      candidate++;
      needle++;
    }
    if (*needle == '\0') {
      const char *end = candidate;
      while (*end != '\0' && *end != '"') {
        end++;
      }
      if (*end == '"') {
        *value_len = (size_t) (end - candidate);
        return candidate;
      }
      return NULL;
    }
    cursor++;
  }
  return NULL;
}

static inline conduit_notice_t conduit_notice_parse(const char *json) {
  conduit_notice_t notice = {CONDUIT_NOTICE_UNKNOWN, NULL, 0, NULL, 0};
  if (json == NULL) {
    return notice;
  }

  size_t type_len = 0;
  const char *type = conduit_json_string_value(json, "\"type\":\"", &type_len);
  if (type == NULL) {
    return notice;
  }

  if (conduit_streq_literal(type, type_len, "started")) {
    notice.type = CONDUIT_NOTICE_STARTED;
    notice.conversation = conduit_json_string_value(json, "\"conversation\":\"", &notice.conversation_len);
  } else if (conduit_streq_literal(type, type_len, "done")) {
    notice.type = CONDUIT_NOTICE_DONE;
  } else if (conduit_streq_literal(type, type_len, "failed")) {
    notice.type = CONDUIT_NOTICE_FAILED;
    notice.error = conduit_json_string_value(json, "\"error\":\"", &notice.error_len);
  }

  return notice;
}

#ifdef __cplusplus
}
#endif

#endif
