#pragma once

#ifdef USE_ESP32

#include "esphome/components/audio/audio.h"
#include "esphome/components/microphone/microphone_source.h"
#include "esphome/components/speaker/speaker.h"
#include "esphome/core/automation.h"
#include "esphome/core/component.h"
#include "esphome/core/defines.h"

#include "esp_event.h"
#include "esp_websocket_client.h"

#include <string>
#include <vector>

namespace esphome::conduit_voice {

class ConduitVoice : public Component {
 public:
  float get_setup_priority() const override { return setup_priority::AFTER_WIFI; }

  void setup() override;
  void loop() override;
  void dump_config() override;

  void set_server(const std::string &server) { this->server_ = server; }
  void set_scheme(const std::string &scheme) { this->scheme_ = scheme; }
  void set_pipeline(const std::string &pipeline) { this->pipeline_ = pipeline; }
  void set_token(const std::string &token) { this->token_ = token; }
  void set_microphone_source(microphone::MicrophoneSource *microphone_source) {
    this->microphone_source_ = microphone_source;
  }
  void set_debug_microphone_source(microphone::MicrophoneSource *debug_microphone_source) {
    this->debug_microphone_source_ = debug_microphone_source;
  }
  void set_speaker(speaker::Speaker *speaker) { this->speaker_ = speaker; }
  void set_debug_assistant_id(const std::string &debug_assistant_id) { this->debug_assistant_id_ = debug_assistant_id; }
  void set_debug_udp_host(const std::string &debug_udp_host) { this->debug_udp_host_ = debug_udp_host; }
  void set_debug_udp_port(uint16_t debug_udp_port) { this->debug_udp_port_ = debug_udp_port; }
  void set_debug_wake_event_url(const std::string &debug_wake_event_url) {
    this->debug_wake_event_url_ = debug_wake_event_url;
  }

  void start();
  void stop();
  void interrupt();
  void wake_debug_event();
  bool is_running() const { return this->state_ != State::IDLE; }

 protected:
  enum class State : uint8_t {
    IDLE,
    CONNECTING,
    STREAMING,
    REPLYING,
    STOPPING,
    FAILED,
  };

  static void websocket_event_handler_(
      void *handler_args,
      esp_event_base_t base,
      int32_t event_id,
      void *event_data);

  void handle_websocket_event_(int32_t event_id, esp_websocket_event_data_t *event);
  void handle_text_frame_(const char *data, size_t length);
  void handle_binary_frame_(const uint8_t *data, size_t length);
  void handle_microphone_data_(const std::vector<uint8_t> &data);
  void handle_debug_microphone_data_(const std::vector<uint8_t> &data);
  void send_debug_udp_(const uint8_t *data, size_t length);
  void cleanup_client_();
  std::string build_url_() const;
  const char *build_headers_();
  void finish_utterance_();
  bool send_command_(const char *json, const char *what);

  std::string server_;
  std::string scheme_{"ws"};
  std::string pipeline_;
  std::string token_;
  // Held as a member because `esp_websocket_client_config_t::headers` borrows
  // the string rather than copying it: a local would be freed before the client
  // sends the upgrade request.
  std::string headers_;
  std::string debug_assistant_id_;
  std::string debug_udp_host_;
  std::string debug_wake_event_url_;
  microphone::MicrophoneSource *microphone_source_{nullptr};
  microphone::MicrophoneSource *debug_microphone_source_{nullptr};
  speaker::Speaker *speaker_{nullptr};
  esp_websocket_client_handle_t client_{nullptr};
  State state_{State::IDLE};
  uint16_t debug_udp_port_{6056};
  uint32_t debug_udp_sequence_{0};
  bool pending_start_{false};
  bool pending_stop_{false};
  bool send_end_on_connect_{false};
};

template<typename... Ts> class StartAction : public Action<Ts...>, public Parented<ConduitVoice> {
 public:
  void play(const Ts &...) override { this->parent_->start(); }
};

template<typename... Ts> class StopAction : public Action<Ts...>, public Parented<ConduitVoice> {
 public:
  void play(const Ts &...) override { this->parent_->stop(); }
};

template<typename... Ts> class InterruptAction : public Action<Ts...>, public Parented<ConduitVoice> {
 public:
  void play(const Ts &...) override { this->parent_->interrupt(); }
};

template<typename... Ts> class IsRunningCondition : public Condition<Ts...>, public Parented<ConduitVoice> {
 public:
  bool check(const Ts &...) override { return this->parent_->is_running(); }
};

template<typename... Ts> class WakeDebugEventAction : public Action<Ts...>, public Parented<ConduitVoice> {
 public:
  void play(const Ts &...) override { this->parent_->wake_debug_event(); }
};

}  // namespace esphome::conduit_voice

#endif  // USE_ESP32
