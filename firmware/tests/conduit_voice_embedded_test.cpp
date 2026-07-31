#include "firmware/esphome/components/conduit_voice/conduit_converse_embedded.h"

#include <cstring>

using esphome::conduit_voice::ConduitNoticeType;
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
  return expect(done.type == ConduitNoticeType::DONE);
}
