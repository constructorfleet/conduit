/*
 * Reference sketch of the Conduit conversation wire contract. Not compiled into
 * any firmware image: the shipping ESPHome firmware uses its own copy,
 * firmware/esphome/components/conduit_voice/conduit_converse_embedded.h.
 * Only firmware/tests/conduit_converse_test.c consumes this header.
 * Canonical protocol definitions live in crates/conduit-core/src/device.rs;
 * agreement between them is maintained by hand and is not checked by CI.
 * See firmware/README.md.
 */
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

static inline int conduit_pipeline_name_is_valid(const char *pipeline) {
  size_t len = 0;
  if (pipeline == NULL || pipeline[0] == '\0') {
    return 0;
  }
  while (pipeline[len] != '\0') {
    const char c = pipeline[len];
    const int ok = (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z') ||
        (c >= '0' && c <= '9') || c == '-' || c == '_';
    if (!ok) {
      return 0;
    }
    len++;
  }
  return len <= 128;
}

static inline size_t conduit_strlen(const char *value) {
  size_t len = 0;
  if (value == NULL) {
    return 0;
  }
  while (value[len] != '\0') {
    len++;
  }
  return len;
}

static inline size_t conduit_copy(char *out, size_t capacity, size_t offset, const char *value) {
  size_t index = 0;
  while (value[index] != '\0') {
    if (out != NULL && offset + index + 1 < capacity) {
      out[offset + index] = value[index];
    }
    index++;
  }
  return offset + index;
}

static inline size_t conduit_converse_path(char *out, size_t capacity, const char *pipeline) {
  if (!conduit_pipeline_name_is_valid(pipeline)) {
    if (out != NULL && capacity > 0) {
      out[0] = '\0';
    }
    return 0;
  }

  size_t len = 0;
  len = conduit_copy(out, capacity, len, CONDUIT_CONVERSE_PATH_PREFIX);
  len = conduit_copy(out, capacity, len, pipeline);
  len = conduit_copy(out, capacity, len, CONDUIT_CONVERSE_PATH_SUFFIX);
  if (out != NULL && capacity > 0) {
    out[len < capacity ? len : capacity - 1] = '\0';
  }
  return len;
}

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
