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

// A conversation id is a UUID, so 36 bytes plus room to notice something
// longer arriving rather than writing past the end of the buffer.
static constexpr size_t CONDUIT_VOICE_NOTICE_MAX_CONVERSATION_BYTES = 48;
// Error text comes from a provider and has no bound of its own. Long enough to
// be worth logging, and truncation is reported rather than hidden.
static constexpr size_t CONDUIT_VOICE_NOTICE_MAX_ERROR_BYTES = 192;

enum class ConduitNoticeType : uint8_t {
  UNKNOWN = 0,
  STARTED,
  DONE,
  FAILED,
};

// Decoded values are copied out rather than pointed at, because a JSON string
// is not its own bytes: `\n` and `\"` in the frame are one character each in
// the value, so there is nothing in the input to point a length at.
struct ConduitNotice {
  ConduitNoticeType type{ConduitNoticeType::UNKNOWN};
  char conversation[CONDUIT_VOICE_NOTICE_MAX_CONVERSATION_BYTES]{};
  size_t conversation_len{0};
  char error[CONDUIT_VOICE_NOTICE_MAX_ERROR_BYTES]{};
  size_t error_len{0};
  // Set when a value did not fit. The value is still usable as far as it goes;
  // a caller that must not report a partial message can check this.
  bool truncated{false};
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

inline const char *conduit_voice_json_skip_space(const char *cursor) {
  while (*cursor == ' ' || *cursor == '\t' || *cursor == '\n' || *cursor == '\r') {
    cursor++;
  }
  return cursor;
}

// Appends one byte, recording that it did not fit rather than writing past the
// end. A silent overrun here would be a buffer overflow driven by a remote
// server's error message.
inline void conduit_voice_json_push(char *out, size_t capacity, size_t *len, bool *truncated,
                                   char byte) {
  if (*len + 1 < capacity) {
    out[*len] = byte;
    (*len)++;
  } else {
    *truncated = true;
  }
}

// Appends a code point as UTF-8. Only reached via `\uXXXX`; anything above
// ASCII already arrives as UTF-8 bytes and is copied through unchanged.
inline void conduit_voice_json_push_utf8(char *out, size_t capacity, size_t *len, bool *truncated,
                                        uint32_t code_point) {
  if (code_point < 0x80) {
    conduit_voice_json_push(out, capacity, len, truncated, static_cast<char>(code_point));
  } else if (code_point < 0x800) {
    conduit_voice_json_push(out, capacity, len, truncated, static_cast<char>(0xC0 | (code_point >> 6)));
    conduit_voice_json_push(out, capacity, len, truncated, static_cast<char>(0x80 | (code_point & 0x3F)));
  } else if (code_point < 0x10000) {
    conduit_voice_json_push(out, capacity, len, truncated, static_cast<char>(0xE0 | (code_point >> 12)));
    conduit_voice_json_push(out, capacity, len, truncated, static_cast<char>(0x80 | ((code_point >> 6) & 0x3F)));
    conduit_voice_json_push(out, capacity, len, truncated, static_cast<char>(0x80 | (code_point & 0x3F)));
  } else {
    conduit_voice_json_push(out, capacity, len, truncated, static_cast<char>(0xF0 | (code_point >> 18)));
    conduit_voice_json_push(out, capacity, len, truncated, static_cast<char>(0x80 | ((code_point >> 12) & 0x3F)));
    conduit_voice_json_push(out, capacity, len, truncated, static_cast<char>(0x80 | ((code_point >> 6) & 0x3F)));
    conduit_voice_json_push(out, capacity, len, truncated, static_cast<char>(0x80 | (code_point & 0x3F)));
  }
}

// Reads four hex digits, or returns false. `out` is untouched on failure.
inline bool conduit_voice_json_hex4(const char *cursor, uint32_t *out) {
  uint32_t value = 0;
  for (size_t i = 0; i < 4; i++) {
    const char c = cursor[i];
    uint32_t digit = 0;
    if (c >= '0' && c <= '9') {
      digit = static_cast<uint32_t>(c - '0');
    } else if (c >= 'a' && c <= 'f') {
      digit = static_cast<uint32_t>(c - 'a') + 10;
    } else if (c >= 'A' && c <= 'F') {
      digit = static_cast<uint32_t>(c - 'A') + 10;
    } else {
      return false;
    }
    value = (value << 4) | digit;
  }
  *out = value;
  return true;
}

// Decodes the JSON string starting at `cursor` (which must be its opening
// quote) into `out`, and returns the position just past its closing quote.
//
// Returns nullptr if the string is malformed or unterminated. Pass a null
// `out` with zero capacity to skip a string without keeping it.
inline const char *conduit_voice_json_read_string(const char *cursor, char *out, size_t capacity,
                                                 size_t *out_len, bool *truncated) {
  size_t len = 0;
  bool overflowed = false;
  if (*cursor != '"') {
    return nullptr;
  }
  cursor++;

  while (*cursor != '\0') {
    if (*cursor == '"') {
      if (out != nullptr && capacity > 0) {
        out[len] = '\0';
      }
      if (out_len != nullptr) {
        *out_len = len;
      }
      if (truncated != nullptr && overflowed) {
        *truncated = true;
      }
      return cursor + 1;
    }

    if (*cursor != '\\') {
      conduit_voice_json_push(out, capacity, &len, &overflowed, *cursor);
      cursor++;
      continue;
    }

    // An escape. This is the whole reason values are decoded rather than
    // pointed at: `\"` inside a value used to end it early, and a value
    // containing `"type":"` used to be read as the notice's own type.
    cursor++;
    switch (*cursor) {
      case '"':
      case '\\':
      case '/':
        conduit_voice_json_push(out, capacity, &len, &overflowed, *cursor);
        cursor++;
        break;
      case 'b':
        conduit_voice_json_push(out, capacity, &len, &overflowed, '\b');
        cursor++;
        break;
      case 'f':
        conduit_voice_json_push(out, capacity, &len, &overflowed, '\f');
        cursor++;
        break;
      case 'n':
        conduit_voice_json_push(out, capacity, &len, &overflowed, '\n');
        cursor++;
        break;
      case 'r':
        conduit_voice_json_push(out, capacity, &len, &overflowed, '\r');
        cursor++;
        break;
      case 't':
        conduit_voice_json_push(out, capacity, &len, &overflowed, '\t');
        cursor++;
        break;
      case 'u': {
        uint32_t code_point = 0;
        if (!conduit_voice_json_hex4(cursor + 1, &code_point)) {
          return nullptr;
        }
        cursor += 5;
        // A character outside the BMP arrives as a surrogate pair.
        if (code_point >= 0xD800 && code_point <= 0xDBFF && cursor[0] == '\\' &&
            cursor[1] == 'u') {
          uint32_t low = 0;
          if (!conduit_voice_json_hex4(cursor + 2, &low)) {
            return nullptr;
          }
          if (low >= 0xDC00 && low <= 0xDFFF) {
            code_point = 0x10000 + ((code_point - 0xD800) << 10) + (low - 0xDC00);
            cursor += 6;
          }
        }
        conduit_voice_json_push_utf8(out, capacity, &len, &overflowed, code_point);
        break;
      }
      default:
        // An escape this parser does not know. Refusing is safer than guessing
        // what the byte after the backslash was supposed to mean.
        return nullptr;
    }
  }

  return nullptr;
}

// Skips one JSON value, whatever its type, and returns the position after it.
//
// Needed so that a member this firmware does not know about — including a
// nested object or array — cannot be mistaken for the frame's own fields.
inline const char *conduit_voice_json_skip_value(const char *cursor) {
  cursor = conduit_voice_json_skip_space(cursor);
  if (*cursor == '"') {
    return conduit_voice_json_read_string(cursor, nullptr, 0, nullptr, nullptr);
  }

  if (*cursor == '{' || *cursor == '[') {
    // Counting brackets is only correct if strings are skipped properly, since
    // a string may contain one.
    size_t depth = 0;
    while (*cursor != '\0') {
      if (*cursor == '"') {
        cursor = conduit_voice_json_read_string(cursor, nullptr, 0, nullptr, nullptr);
        if (cursor == nullptr) {
          return nullptr;
        }
        continue;
      }
      if (*cursor == '{' || *cursor == '[') {
        depth++;
      } else if (*cursor == '}' || *cursor == ']') {
        depth--;
        if (depth == 0) {
          return cursor + 1;
        }
      }
      cursor++;
    }
    return nullptr;
  }

  // A number, `true`, `false`, or `null`: everything up to whatever ends it.
  const char *start = cursor;
  while (*cursor != '\0' && *cursor != ',' && *cursor != '}' && *cursor != ']' &&
         *cursor != ' ' && *cursor != '\t' && *cursor != '\n' && *cursor != '\r') {
    cursor++;
  }
  return cursor == start ? nullptr : cursor;
}

inline ConduitNotice conduit_voice_notice_parse(const char *json) {
  ConduitNotice notice;
  if (json == nullptr) {
    return notice;
  }

  const char *cursor = conduit_voice_json_skip_space(json);
  // Only an object is a notice. An array or a bare value is not one, and
  // scanning it for a key pattern would find one in the wrong place.
  if (*cursor != '{') {
    return notice;
  }
  cursor = conduit_voice_json_skip_space(cursor + 1);
  if (*cursor == '}') {
    return notice;
  }

  char type[16] = {};
  size_t type_len = 0;
  bool have_type = false;

  while (*cursor != '\0') {
    char key[32] = {};
    size_t key_len = 0;
    bool key_truncated = false;
    cursor = conduit_voice_json_skip_space(cursor);
    cursor = conduit_voice_json_read_string(cursor, key, sizeof(key), &key_len, &key_truncated);
    if (cursor == nullptr) {
      return ConduitNotice{};
    }

    cursor = conduit_voice_json_skip_space(cursor);
    if (*cursor != ':') {
      return ConduitNotice{};
    }
    cursor = conduit_voice_json_skip_space(cursor + 1);

    const bool known_key =
        !key_truncated && (conduit_voice_streq_literal(key, key_len, "type") ||
                           conduit_voice_streq_literal(key, key_len, "conversation") ||
                           conduit_voice_streq_literal(key, key_len, "error"));

    if (known_key && *cursor == '"') {
      char *out = type;
      size_t capacity = sizeof(type);
      size_t *len = &type_len;
      if (conduit_voice_streq_literal(key, key_len, "conversation")) {
        out = notice.conversation;
        capacity = sizeof(notice.conversation);
        len = &notice.conversation_len;
      } else if (conduit_voice_streq_literal(key, key_len, "error")) {
        out = notice.error;
        capacity = sizeof(notice.error);
        len = &notice.error_len;
      } else {
        have_type = true;
      }
      cursor = conduit_voice_json_read_string(cursor, out, capacity, len, &notice.truncated);
    } else {
      cursor = conduit_voice_json_skip_value(cursor);
    }
    if (cursor == nullptr) {
      return ConduitNotice{};
    }

    cursor = conduit_voice_json_skip_space(cursor);
    if (*cursor == ',') {
      cursor++;
      continue;
    }
    if (*cursor == '}') {
      break;
    }
    return ConduitNotice{};
  }

  if (!have_type) {
    return ConduitNotice{};
  }

  if (conduit_voice_streq_literal(type, type_len, "started")) {
    notice.type = ConduitNoticeType::STARTED;
  } else if (conduit_voice_streq_literal(type, type_len, "done")) {
    notice.type = ConduitNoticeType::DONE;
  } else if (conduit_voice_streq_literal(type, type_len, "failed")) {
    notice.type = ConduitNoticeType::FAILED;
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
