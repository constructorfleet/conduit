// Drives the shipped firmware notice parser against the exact frames the
// server emits.
//
// The fixture is generated from the canonical Rust definitions, so this is a
// check that the firmware can read what the server writes — not merely that
// the two spell the protocol the same way. See firmware/README.md.

#include "firmware/esphome/components/conduit_voice/conduit_converse_embedded.h"

#include <cstdio>
#include <cstring>
#include <fstream>
#include <sstream>
#include <string>
#include <vector>

using esphome::conduit_voice::ConduitNotice;
using esphome::conduit_voice::ConduitNoticeType;
using esphome::conduit_voice::conduit_voice_notice_parse;

namespace {

const char *const FIXTURE = "firmware/tests/notices.fixture";
const char *const FIXTURE_VERSION = "conduit-notice-fixture 1";

int failures = 0;

void fail(const std::string &name, const std::string &detail) {
  std::fprintf(stderr, "%s: %s\n", name.c_str(), detail.c_str());
  failures++;
}

void expect_eq(const std::string &name, const std::string &what, const std::string &actual,
               const std::string &expected) {
  if (actual != expected) {
    fail(name, what + ": expected [" + expected + "], got [" + actual + "]");
  }
}

// Reverses the fixture's escaping. Anything else is a malformed fixture rather
// than a parser failure, so it stops the run.
bool unescape(const std::string &value, std::string *out) {
  out->clear();
  for (size_t i = 0; i < value.size(); i++) {
    if (value[i] != '\\') {
      out->push_back(value[i]);
      continue;
    }
    if (i + 1 >= value.size()) {
      return false;
    }
    i++;
    switch (value[i]) {
      case '\\': out->push_back('\\'); break;
      case 't': out->push_back('\t'); break;
      case 'n': out->push_back('\n'); break;
      case 'r': out->push_back('\r'); break;
      default: return false;
    }
  }
  return true;
}

std::vector<std::string> split_tabs(const std::string &line) {
  std::vector<std::string> fields;
  std::string field;
  std::istringstream stream(line);
  while (std::getline(stream, field, '\t')) {
    fields.push_back(field);
  }
  return fields;
}

std::string type_name(ConduitNoticeType type) {
  switch (type) {
    case ConduitNoticeType::STARTED: return "started";
    case ConduitNoticeType::DONE: return "done";
    case ConduitNoticeType::FAILED: return "failed";
    case ConduitNoticeType::UNKNOWN: return "unknown";
  }
  return "unrecognized";
}

// Checks one fixture record. Returns false only if the record itself is
// unreadable, which is a broken fixture rather than a failing assertion.
bool check(const std::string &line) {
  const std::vector<std::string> fields = split_tabs(line);
  if (fields.size() < 3) {
    std::fprintf(stderr, "fixture record needs at least 3 fields: %s\n", line.c_str());
    return false;
  }

  const std::string &name = fields[0];
  std::string frame;
  if (!unescape(fields[1], &frame)) {
    std::fprintf(stderr, "%s: unreadable frame escaping\n", name.c_str());
    return false;
  }

  const ConduitNotice notice = conduit_voice_notice_parse(frame.c_str());
  expect_eq(name, "notice type", type_name(notice.type), fields[2]);
  if (notice.truncated) {
    fail(name, "a canonical frame must not overflow the parser's buffers");
  }

  for (size_t i = 3; i < fields.size(); i++) {
    const size_t equals = fields[i].find('=');
    if (equals == std::string::npos) {
      std::fprintf(stderr, "%s: field without '=': %s\n", name.c_str(), fields[i].c_str());
      return false;
    }
    const std::string key = fields[i].substr(0, equals);
    std::string expected;
    if (!unescape(fields[i].substr(equals + 1), &expected)) {
      std::fprintf(stderr, "%s: unreadable field escaping\n", name.c_str());
      return false;
    }

    if (key == "conversation") {
      expect_eq(name, "conversation", std::string(notice.conversation, notice.conversation_len),
                expected);
    } else if (key == "error") {
      expect_eq(name, "error", std::string(notice.error, notice.error_len), expected);
    } else {
      std::fprintf(stderr, "%s: unknown fixture field '%s'\n", name.c_str(), key.c_str());
      return false;
    }
  }

  return true;
}

// Cases that cannot come from the fixture, because the server cannot produce
// them: a truncated frame, a hostile one, a frame that is not JSON at all.
void check_frames_the_server_would_never_send() {
  struct Case {
    const char *name;
    const char *frame;
    ConduitNoticeType expected;
  };
  const Case cases[] = {
      {"empty", "", ConduitNoticeType::UNKNOWN},
      {"not json", "done", ConduitNoticeType::UNKNOWN},
      {"empty object", "{}", ConduitNoticeType::UNKNOWN},
      {"unterminated frame", "{\"type\":\"done", ConduitNoticeType::UNKNOWN},
      {"unterminated escape", "{\"type\":\"done\\", ConduitNoticeType::UNKNOWN},
      {"missing colon", "{\"type\" \"done\"}", ConduitNoticeType::UNKNOWN},
      {"missing type", "{\"error\":\"nope\"}", ConduitNoticeType::UNKNOWN},
      {"type is not a string", "{\"type\":7}", ConduitNoticeType::UNKNOWN},
      // The escaped value must not be read as this frame's own type. Before
      // values were decoded rather than pointed at, this parsed as `done`.
      {"type hidden in an error", "{\"type\":\"failed\",\"error\":\"\\\"type\\\":\\\"done\\\"\"}",
       ConduitNoticeType::FAILED},
      // A nested object must be skipped whole, brackets inside strings and all.
      {"nested object with a decoy", "{\"detail\":{\"note\":\"}\\\"type\\\":\\\"done\\\"\"},\"type\":\"failed\"}",
       ConduitNoticeType::FAILED},
      {"unknown escape", "{\"type\":\"failed\",\"error\":\"\\x\"}", ConduitNoticeType::UNKNOWN},
      {"bad unicode escape", "{\"type\":\"failed\",\"error\":\"\\u00zz\"}",
       ConduitNoticeType::UNKNOWN},
  };

  for (const Case &test : cases) {
    const ConduitNotice notice = conduit_voice_notice_parse(test.frame);
    expect_eq(test.name, "notice type", type_name(notice.type), type_name(test.expected));
  }

  // A null frame must not be dereferenced.
  if (conduit_voice_notice_parse(nullptr).type != ConduitNoticeType::UNKNOWN) {
    fail("null frame", "expected an unknown notice");
  }

  // An escaped unicode sequence decodes rather than being copied literally.
  const ConduitNotice unicode =
      conduit_voice_notice_parse("{\"type\":\"failed\",\"error\":\"caf\\u00e9 \\ud83d\\ude00\"}");
  expect_eq("unicode escape", "error", std::string(unicode.error, unicode.error_len),
            "caf\xc3\xa9 \xf0\x9f\x98\x80");

  // Error text is unbounded upstream, so a long one must truncate and say so
  // rather than overflow a fixed buffer.
  std::string long_error = "{\"type\":\"failed\",\"error\":\"";
  long_error.append(4096, 'x');
  long_error.append("\"}");
  const ConduitNotice truncated = conduit_voice_notice_parse(long_error.c_str());
  if (truncated.type != ConduitNoticeType::FAILED) {
    fail("long error", "a long error must still parse as failed");
  }
  if (!truncated.truncated) {
    fail("long error", "expected the notice to report truncation");
  }
  if (truncated.error_len >= sizeof(truncated.error)) {
    fail("long error", "the decoded error must stay inside its buffer");
  }
}

}  // namespace

int main(int argc, char **argv) {
  // The fixture path is repo-relative, so the runner passes a root rather than
  // this test guessing at a working directory.
  const std::string root = argc > 1 ? argv[1] : ".";
  const std::string path = root + "/" + FIXTURE;

  std::ifstream fixture(path);
  if (!fixture) {
    std::fprintf(stderr, "cannot read %s\n", path.c_str());
    return 1;
  }

  std::string line;
  if (!std::getline(fixture, line) || line != FIXTURE_VERSION) {
    std::fprintf(stderr, "%s: expected first line [%s], got [%s]\n", path.c_str(),
                 FIXTURE_VERSION, line.c_str());
    return 1;
  }

  size_t records = 0;
  while (std::getline(fixture, line)) {
    if (line.empty() || line[0] == '#') {
      continue;
    }
    if (!check(line)) {
      return 1;
    }
    records++;
  }

  if (records == 0) {
    std::fprintf(stderr, "%s: no records; a silently empty fixture checks nothing\n",
                 path.c_str());
    return 1;
  }

  check_frames_the_server_would_never_send();

  if (failures != 0) {
    std::fprintf(stderr, "%d assertion(s) failed across %zu fixture record(s)\n", failures,
                 records);
    return 1;
  }
  std::printf("%zu fixture record(s) parsed as the server intends\n", records);
  return 0;
}
