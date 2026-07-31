#include "conduit_voice.h"

#ifdef USE_ESP32

#include "esphome/core/log.h"
#include "esphome/core/hal.h"

#include "esp_http_client.h"
#include "lwip/netdb.h"
#include "lwip/sockets.h"

#include <cstring>
#include <unistd.h>

namespace esphome::conduit_voice {

static const char *const TAG = "conduit_voice";

void ConduitVoice::setup() {
  if (this->microphone_source_ == nullptr) {
    ESP_LOGE(TAG, "No microphone source configured");
    this->mark_failed();
    return;
  }
  if (this->speaker_ == nullptr) {
    ESP_LOGE(TAG, "No speaker configured");
    this->mark_failed();
    return;
  }
  if (!conduit_voice_pipeline_name_is_valid(this->pipeline_.c_str())) {
    ESP_LOGE(TAG, "Invalid pipeline name: %s", this->pipeline_.c_str());
    this->mark_failed();
    return;
  }

  this->microphone_source_->add_data_callback([this](const std::vector<uint8_t> &data) {
    this->handle_microphone_data_(data);
  });
  if (this->debug_microphone_source_ != nullptr) {
    this->debug_microphone_source_->add_data_callback([this](const std::vector<uint8_t> &data) {
      this->handle_debug_microphone_data_(data);
    });
  }

  audio::AudioStreamInfo stream_info(
      CONDUIT_VOICE_AUDIO_BITS_PER_SAMPLE,
      CONDUIT_VOICE_AUDIO_CHANNELS,
      CONDUIT_VOICE_AUDIO_SAMPLE_RATE_HZ);
  this->speaker_->set_audio_stream_info(stream_info);
}

void ConduitVoice::loop() {
  if (this->state_.load() == State::STREAMING &&
      conduit_voice_utterance_timeout_elapsed(
          millis(), this->utterance_started_at_ms_, this->max_utterance_ms_)) {
    ESP_LOGD(TAG, "Ending Conduit utterance after %u ms",
             static_cast<unsigned>(this->max_utterance_ms_));
    this->finish_utterance_();
  }
  if (this->pending_stop_.exchange(false)) {
    this->cleanup_client_();
    if (this->speaker_ != nullptr && this->speaker_->is_running()) {
      this->speaker_->finish();
    }
    this->state_.store(State::IDLE);
  }
}

void ConduitVoice::dump_config() {
  ESP_LOGCONFIG(TAG, "Conduit Voice:");
  ESP_LOGCONFIG(TAG, "  Server: %s", this->server_.c_str());
  ESP_LOGCONFIG(TAG, "  Scheme: %s", this->scheme_.c_str());
  ESP_LOGCONFIG(TAG, "  Pipeline: %s", this->pipeline_.c_str());
  // Whether a credential is configured, never the credential: this log is
  // printed at every boot and is the first thing anyone pastes into an issue.
  ESP_LOGCONFIG(TAG, "  Token: %s", this->token_.empty() ? "not set" : "set");
}

void ConduitVoice::start() {
  if (this->is_failed()) {
    ESP_LOGE(TAG, "Cannot start failed Conduit voice component");
    return;
  }
  if (this->state_.load() != State::IDLE) {
    ESP_LOGW(TAG, "Conduit voice session is already running");
    return;
  }

  const std::string url = this->build_url_();
  esp_websocket_client_config_t config = {};
  config.uri = url.c_str();
  // Borrowed, not copied — `headers_` outlives the client, `url` only has to
  // outlive this call.
  config.headers = this->build_headers_();
  config.network_timeout_ms = 10000;
  config.reconnect_timeout_ms = 1000;
  config.buffer_size = 4096;
  config.user_context = this;

  this->client_ = esp_websocket_client_init(&config);
  if (this->client_ == nullptr) {
    ESP_LOGE(TAG, "Failed to create WebSocket client for %s", url.c_str());
    this->state_.store(State::FAILED);
    this->pending_stop_.store(true);
    return;
  }

  esp_err_t err = esp_websocket_register_events(
      this->client_,
      WEBSOCKET_EVENT_ANY,
      &ConduitVoice::websocket_event_handler_,
      this);
  if (err != ESP_OK) {
    ESP_LOGE(TAG, "Failed to register WebSocket events: %s", esp_err_to_name(err));
    this->pending_stop_.store(true);
    return;
  }

  this->state_.store(State::CONNECTING);
  err = esp_websocket_client_start(this->client_);
  if (err != ESP_OK) {
    ESP_LOGE(TAG, "Failed to connect to Conduit at %s: %s", url.c_str(), esp_err_to_name(err));
    this->pending_stop_.store(true);
  }
}

void ConduitVoice::stop() {
  if (this->state_.load() == State::IDLE) {
    return;
  }
  this->finish_utterance_();
}

void ConduitVoice::interrupt() {
  const State state = this->state_.load();
  if (state == State::IDLE) {
    return;
  }
  // Nothing has been sent yet, so there is no turn to interrupt — but the
  // socket is opening, and dropping it would look like a device that vanished.
  // Say `end` on connect and let the turn finish; a caller who wants silence
  // now should use `stop`.
  if (state == State::CONNECTING) {
    this->send_end_on_connect_.store(true);
    return;
  }

  if (this->microphone_source_ != nullptr && this->microphone_source_->is_running()) {
    this->microphone_source_->stop();
  }
  // Stop the speaker locally as well as asking the server: audio already handed
  // to the speaker would otherwise keep playing after the reply was cancelled.
  if (this->speaker_ != nullptr && this->speaker_->is_running()) {
    this->speaker_->stop();
  }

  if (!this->send_command_(CONDUIT_VOICE_CONVERSE_STOP_JSON, "stop")) {
    return;
  }
  this->state_.store(State::STOPPING);
}

void ConduitVoice::wake_debug_event() {
  if (this->debug_wake_event_url_.empty()) {
    return;
  }

  std::string url = this->debug_wake_event_url_;
  url += (url.find('?') == std::string::npos) ? "?" : "&";
  url += "assistant_id=" + this->debug_assistant_id_;

  esp_http_client_config_t config = {};
  config.url = url.c_str();
  config.method = HTTP_METHOD_POST;
  config.timeout_ms = 3000;

  esp_http_client_handle_t client = esp_http_client_init(&config);
  if (client == nullptr) {
    ESP_LOGW(TAG, "Failed to create wake debug HTTP client");
    return;
  }
  esp_err_t err = esp_http_client_perform(client);
  if (err != ESP_OK) {
    ESP_LOGW(TAG, "Failed to post wake debug event: %s", esp_err_to_name(err));
  }
  esp_http_client_cleanup(client);
}

void ConduitVoice::websocket_event_handler_(
    void *handler_args,
    esp_event_base_t base,
    int32_t event_id,
    void *event_data) {
  (void) base;
  auto *self = static_cast<ConduitVoice *>(handler_args);
  auto *event = static_cast<esp_websocket_event_data_t *>(event_data);
  if (self != nullptr) {
    self->handle_websocket_event_(event_id, event);
  }
}

void ConduitVoice::handle_websocket_event_(int32_t event_id, esp_websocket_event_data_t *event) {
  switch (event_id) {
    case WEBSOCKET_EVENT_CONNECTED:
      ESP_LOGD(TAG, "Connected to Conduit");
      this->state_.store(State::STREAMING);
      this->utterance_started_at_ms_ = millis();
      if (this->microphone_source_ != nullptr) {
        this->microphone_source_->start();
      }
      if (this->send_end_on_connect_.exchange(false)) {
        this->finish_utterance_();
      }
      break;
    case WEBSOCKET_EVENT_DISCONNECTED:
      ESP_LOGD(TAG, "Disconnected from Conduit");
      this->pending_stop_.store(true);
      break;
    case WEBSOCKET_EVENT_DATA:
      if (event == nullptr || event->data_ptr == nullptr || event->data_len <= 0) {
        return;
      }
      if (event->op_code == 0x1) {
        this->handle_text_frame_(event->data_ptr, static_cast<size_t>(event->data_len));
      } else if (event->op_code == 0x2) {
        this->handle_binary_frame_(reinterpret_cast<const uint8_t *>(event->data_ptr),
                                   static_cast<size_t>(event->data_len));
      }
      break;
    case WEBSOCKET_EVENT_ERROR:
      ESP_LOGE(TAG, "Conduit WebSocket error");
      this->state_.store(State::FAILED);
      this->pending_stop_.store(true);
      break;
    default:
      break;
  }
}

void ConduitVoice::handle_text_frame_(const char *data, size_t length) {
  std::string text(data, length);
  ConduitNotice notice = conduit_voice_notice_parse(text.c_str());
  switch (notice.type) {
    case ConduitNoticeType::STARTED:
      ESP_LOGD(TAG, "Conduit conversation started: conversation=%s", notice.conversation);
      break;
    case ConduitNoticeType::DONE:
      ESP_LOGD(TAG, "Conduit conversation done");
      this->pending_stop_.store(true);
      break;
    case ConduitNoticeType::FAILED:
      // The server's reason is the only diagnosis available on-device, so log
      // it rather than making someone read the server to learn what broke.
      ESP_LOGE(TAG, "Conduit conversation failed: error=%s%s", notice.error,
               notice.truncated ? " (truncated)" : "");
      this->state_.store(State::FAILED);
      this->pending_stop_.store(true);
      break;
    case ConduitNoticeType::UNKNOWN:
      ESP_LOGW(TAG, "Unknown Conduit notice: %s", text.c_str());
      break;
  }
}

void ConduitVoice::handle_binary_frame_(const uint8_t *data, size_t length) {
  if (this->speaker_ == nullptr || data == nullptr || length == 0) {
    return;
  }
  if (!this->speaker_->is_running()) {
    this->speaker_->start();
  }
  this->state_.store(State::REPLYING);
  size_t written = this->speaker_->play(data, length, pdMS_TO_TICKS(100));
  if (written < length) {
    ESP_LOGW(TAG, "Speaker accepted %u of %u reply bytes", static_cast<unsigned>(written),
             static_cast<unsigned>(length));
  }
}

void ConduitVoice::handle_microphone_data_(const std::vector<uint8_t> &data) {
  if (this->state_.load() != State::STREAMING || this->client_ == nullptr || data.empty()) {
    return;
  }
  if (!esp_websocket_client_is_connected(this->client_)) {
    return;
  }

  int written = esp_websocket_client_send_bin(
      this->client_,
      reinterpret_cast<const char *>(data.data()),
      data.size(),
      pdMS_TO_TICKS(100));
  if (written < 0) {
    ESP_LOGE(TAG, "Failed to send microphone audio to Conduit");
    this->state_.store(State::FAILED);
    this->pending_stop_.store(true);
  }
}

void ConduitVoice::handle_debug_microphone_data_(const std::vector<uint8_t> &data) {
  if (this->debug_udp_host_.empty() || this->debug_assistant_id_.empty() || data.empty()) {
    return;
  }
  this->send_debug_udp_(data.data(), data.size());
}

void ConduitVoice::send_debug_udp_(const uint8_t *data, size_t length) {
  const size_t assistant_id_len = this->debug_assistant_id_.size();
  std::vector<uint8_t> packet(CONDUIT_VOICE_WWD2_HEADER_BYTES + assistant_id_len + length);
  const size_t packet_len = conduit_voice_wwd2_packet(
      packet.data(),
      packet.size(),
      this->debug_assistant_id_.c_str(),
      data,
      length,
      this->debug_udp_sequence_);
  if (packet_len == 0) {
    return;
  }

  struct addrinfo hints = {};
  hints.ai_family = AF_INET;
  hints.ai_socktype = SOCK_DGRAM;
  struct addrinfo *result = nullptr;
  const std::string port = std::to_string(this->debug_udp_port_);
  if (getaddrinfo(this->debug_udp_host_.c_str(), port.c_str(), &hints, &result) != 0 || result == nullptr) {
    ESP_LOGW(TAG, "Failed to resolve wake debug UDP host %s", this->debug_udp_host_.c_str());
    return;
  }

  int fd = socket(result->ai_family, result->ai_socktype, result->ai_protocol);
  if (fd < 0) {
    freeaddrinfo(result);
    return;
  }
  ssize_t sent = sendto(fd, packet.data(), packet_len, 0, result->ai_addr, result->ai_addrlen);
  close(fd);
  freeaddrinfo(result);
  if (sent == static_cast<ssize_t>(packet_len)) {
    this->debug_udp_sequence_++;
  }
}

void ConduitVoice::cleanup_client_() {
  if (this->microphone_source_ != nullptr && this->microphone_source_->is_running()) {
    this->microphone_source_->stop();
  }
  if (this->client_ != nullptr) {
    if (esp_websocket_client_is_connected(this->client_)) {
      esp_websocket_client_stop(this->client_);
    }
    esp_websocket_client_destroy(this->client_);
    this->client_ = nullptr;
  }
  this->send_end_on_connect_.store(false);
  this->utterance_started_at_ms_ = 0;
}

std::string ConduitVoice::build_url_() const {
  char path[192];
  conduit_voice_converse_path(path, sizeof(path), this->pipeline_.c_str());
  return this->scheme_ + "://" + this->server_ + path;
}

// Builds the extra upgrade-request headers, returning nullptr when there are
// none. The credential goes here rather than in the URL because the URL is
// logged on two failure paths above and recorded into the server's trace spans.
const char *ConduitVoice::build_headers_() {
  if (this->token_.empty()) {
    return nullptr;
  }
  // The client wants one raw block of CRLF-terminated header lines.
  this->headers_ = "Authorization: Bearer " + this->token_ + "\r\n";
  return this->headers_.c_str();
}

void ConduitVoice::finish_utterance_() {
  if (this->state_.load() == State::CONNECTING) {
    this->send_end_on_connect_.store(true);
    return;
  }

  if (this->microphone_source_ != nullptr && this->microphone_source_->is_running()) {
    this->microphone_source_->stop();
  }
  this->utterance_started_at_ms_ = 0;

  if (!this->send_command_(CONDUIT_VOICE_CONVERSE_END_JSON, "end-of-utterance")) {
    return;
  }
  this->state_.store(State::STOPPING);
}

// Sends one control frame. Returns false if there was nothing to send it on or
// the write failed, having already arranged for the session to end.
bool ConduitVoice::send_command_(const char *json, const char *what) {
  if (this->client_ == nullptr || !esp_websocket_client_is_connected(this->client_)) {
    this->pending_stop_.store(true);
    return false;
  }

  int written = esp_websocket_client_send_text(
      this->client_,
      json,
      std::strlen(json),
      pdMS_TO_TICKS(100));
  if (written < 0) {
    ESP_LOGE(TAG, "Failed to send %s to Conduit", what);
    this->pending_stop_.store(true);
    return false;
  }
  return true;
}

}  // namespace esphome::conduit_voice

#endif  // USE_ESP32
