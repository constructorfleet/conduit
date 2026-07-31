#pragma once

#include <cstddef>
#include <cstdint>
#include <cstring>

namespace esphome::conduit_voice {

static constexpr int CONDUIT_VOICE_AUDIO_SAMPLE_RATE_HZ = 16000;
static constexpr int CONDUIT_VOICE_AUDIO_CHANNELS = 1;
static constexpr int CONDUIT_VOICE_AUDIO_BITS_PER_SAMPLE = 16;
static constexpr uint8_t CONDUIT_VOICE_WWD2_AUDIO_ENCODING_PCM_SIGNED_LE = 1;
static constexpr size_t CONDUIT_VOICE_WWD2_HEADER_BYTES = 18;
static constexpr size_t CONDUIT_VOICE_WWD2_MAX_ASSISTANT_ID_BYTES = 64;
static constexpr size_t CONDUIT_VOICE_WWD2_MAX_PAYLOAD_BYTES = 0xFFFF;
static constexpr const char *CONDUIT_VOICE_CONVERSE_END_JSON = "{\"type\":\"end\"}";
static constexpr const char *CONDUIT_VOICE_CONVERSE_PATH_PREFIX = "/v1/pipelines/";
static constexpr const char *CONDUIT_VOICE_CONVERSE_PATH_SUFFIX = "/converse";

enum class ConduitNoticeType : uint8_t {
  UNKNOWN = 0,
  STARTED,
  DONE,
  FAILED,
};

struct ConduitNotice {
  ConduitNoticeType type{ConduitNoticeType::UNKNOWN};
  const char *conversation{nullptr};
  size_t conversation_len{0};
  const char *error{nullptr};
  size_t error_len{0};
};

inline bool conduit_voice_pipeline_name_is_valid(const char *pipeline) {
  size_t len = 0;
  if (pipeline == nullptr || pipeline[0] == '\0') {
    return false;
  }
  while (pipeline[len] != '\0') {
    const char c = pipeline[len];
    const bool ok = (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z') ||
                    (c >= '0' && c <= '9') || c == '-' || c == '_';
    if (!ok) {
      return false;
    }
    len++;
  }
  return len <= 128;
}

inline size_t conduit_voice_copy(char *out, size_t capacity, size_t offset, const char *value) {
  size_t index = 0;
  while (value[index] != '\0') {
    if (out != nullptr && offset + index + 1 < capacity) {
      out[offset + index] = value[index];
    }
    index++;
  }
  return offset + index;
}

inline size_t conduit_voice_converse_path(char *out, size_t capacity, const char *pipeline) {
  if (!conduit_voice_pipeline_name_is_valid(pipeline)) {
    if (out != nullptr && capacity > 0) {
      out[0] = '\0';
    }
    return 0;
  }

  size_t len = 0;
  len = conduit_voice_copy(out, capacity, len, CONDUIT_VOICE_CONVERSE_PATH_PREFIX);
  len = conduit_voice_copy(out, capacity, len, pipeline);
  len = conduit_voice_copy(out, capacity, len, CONDUIT_VOICE_CONVERSE_PATH_SUFFIX);
  if (out != nullptr && capacity > 0) {
    out[len < capacity ? len : capacity - 1] = '\0';
  }
  return len;
}

inline bool conduit_voice_streq_literal(const char *value, size_t len, const char *literal) {
  size_t i = 0;
  while (literal[i] != '\0') {
    if (i >= len || value[i] != literal[i]) {
      return false;
    }
    i++;
  }
  return i == len;
}

inline const char *conduit_voice_json_string_value(const char *json, const char *key, size_t *value_len) {
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
        *value_len = static_cast<size_t>(end - candidate);
        return candidate;
      }
      return nullptr;
    }
    cursor++;
  }
  return nullptr;
}

inline ConduitNotice conduit_voice_notice_parse(const char *json) {
  ConduitNotice notice;
  if (json == nullptr) {
    return notice;
  }

  size_t type_len = 0;
  const char *type = conduit_voice_json_string_value(json, "\"type\":\"", &type_len);
  if (type == nullptr) {
    return notice;
  }

  if (conduit_voice_streq_literal(type, type_len, "started")) {
    notice.type = ConduitNoticeType::STARTED;
    notice.conversation = conduit_voice_json_string_value(json, "\"conversation\":\"", &notice.conversation_len);
  } else if (conduit_voice_streq_literal(type, type_len, "done")) {
    notice.type = ConduitNoticeType::DONE;
  } else if (conduit_voice_streq_literal(type, type_len, "failed")) {
    notice.type = ConduitNoticeType::FAILED;
    notice.error = conduit_voice_json_string_value(json, "\"error\":\"", &notice.error_len);
  }

  return notice;
}

inline size_t conduit_voice_wwd2_packet(
    uint8_t *out,
    size_t capacity,
    const char *assistant_id,
    const uint8_t *pcm,
    size_t pcm_len,
    uint32_t sequence) {
  if (out == nullptr || assistant_id == nullptr || pcm == nullptr || pcm_len == 0 ||
      pcm_len > CONDUIT_VOICE_WWD2_MAX_PAYLOAD_BYTES) {
    return 0;
  }

  size_t assistant_id_len = 0;
  while (assistant_id[assistant_id_len] != '\0') {
    assistant_id_len++;
  }
  if (assistant_id_len == 0 || assistant_id_len > CONDUIT_VOICE_WWD2_MAX_ASSISTANT_ID_BYTES) {
    return 0;
  }

  const size_t packet_len = CONDUIT_VOICE_WWD2_HEADER_BYTES + assistant_id_len + pcm_len;
  if (capacity < packet_len) {
    return 0;
  }

  out[0] = 'W';
  out[1] = 'W';
  out[2] = 'D';
  out[3] = '2';
  out[4] = static_cast<uint8_t>(assistant_id_len);
  out[5] = CONDUIT_VOICE_AUDIO_CHANNELS;
  out[6] = CONDUIT_VOICE_AUDIO_BITS_PER_SAMPLE;
  out[7] = CONDUIT_VOICE_WWD2_AUDIO_ENCODING_PCM_SIGNED_LE;
  out[8] = static_cast<uint8_t>((CONDUIT_VOICE_AUDIO_SAMPLE_RATE_HZ >> 24) & 0xFF);
  out[9] = static_cast<uint8_t>((CONDUIT_VOICE_AUDIO_SAMPLE_RATE_HZ >> 16) & 0xFF);
  out[10] = static_cast<uint8_t>((CONDUIT_VOICE_AUDIO_SAMPLE_RATE_HZ >> 8) & 0xFF);
  out[11] = static_cast<uint8_t>(CONDUIT_VOICE_AUDIO_SAMPLE_RATE_HZ & 0xFF);
  out[12] = static_cast<uint8_t>((sequence >> 24) & 0xFF);
  out[13] = static_cast<uint8_t>((sequence >> 16) & 0xFF);
  out[14] = static_cast<uint8_t>((sequence >> 8) & 0xFF);
  out[15] = static_cast<uint8_t>(sequence & 0xFF);
  out[16] = static_cast<uint8_t>((pcm_len >> 8) & 0xFF);
  out[17] = static_cast<uint8_t>(pcm_len & 0xFF);
  std::memcpy(out + CONDUIT_VOICE_WWD2_HEADER_BYTES, assistant_id, assistant_id_len);
  std::memcpy(out + CONDUIT_VOICE_WWD2_HEADER_BYTES + assistant_id_len, pcm, pcm_len);
  return packet_len;
}

}  // namespace esphome::conduit_voice
