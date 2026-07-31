#include "firmware/esphome/components/conduit_voice/conduit_converse_embedded.h"

#include <cstring>

using esphome::conduit_voice::ConduitNoticeType;
using esphome::conduit_voice::conduit_voice_wwd2_packet;
using esphome::conduit_voice::conduit_voice_converse_path;
using esphome::conduit_voice::conduit_voice_notice_parse;
using esphome::conduit_voice::conduit_voice_pipeline_name_is_valid;

static int expect(bool ok) { return ok ? 0 : 1; }

int main() {
  char path[80];
  size_t len = conduit_voice_converse_path(path, sizeof(path), "kitchen");
  if (expect(std::strcmp(path, "/v1/pipelines/kitchen/converse") == 0) != 0) {
    return 1;
  }
  if (expect(len == std::strlen("/v1/pipelines/kitchen/converse")) != 0) {
    return 1;
  }
  if (expect(!conduit_voice_pipeline_name_is_valid("../kitchen")) != 0) {
    return 1;
  }

  auto started = conduit_voice_notice_parse("{\"type\":\"started\",\"conversation\":\"abc\"}");
  if (expect(started.type == ConduitNoticeType::STARTED) != 0) {
    return 1;
  }

  auto done = conduit_voice_notice_parse("{\"type\":\"done\"}");
  if (expect(done.type == ConduitNoticeType::DONE) != 0) {
    return 1;
  }

  uint8_t packet[64] = {};
  const uint8_t pcm[] = {0x01, 0x02, 0x03, 0x04};
  const size_t packet_len = conduit_voice_wwd2_packet(packet, sizeof(packet), "kitchen", pcm, sizeof(pcm), 0x01020304);
  if (expect(packet_len == 18 + std::strlen("kitchen") + sizeof(pcm)) != 0) {
    return 1;
  }
  const uint8_t expected_header[] = {
      'W', 'W', 'D', '2',
      7,
      1,
      16,
      1,
      0x00, 0x00, 0x3E, 0x80,
      0x01, 0x02, 0x03, 0x04,
      0x00, 0x04,
  };
  if (expect(std::memcmp(packet, expected_header, sizeof(expected_header)) == 0) != 0) {
    return 1;
  }
  if (expect(std::memcmp(packet + 18, "kitchen", std::strlen("kitchen")) == 0) != 0) {
    return 1;
  }
  if (expect(std::memcmp(packet + 18 + std::strlen("kitchen"), pcm, sizeof(pcm)) == 0) != 0) {
    return 1;
  }

  return 0;
}
