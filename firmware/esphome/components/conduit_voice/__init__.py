from esphome import automation
import esphome.codegen as cg
from esphome.components import esp32, microphone, network, socket, speaker, wifi
import esphome.config_validation as cv
from esphome.const import CONF_ID, CONF_MICROPHONE, CONF_SPEAKER
from esphome.core import ID
from esphome.cpp_generator import TemplateArgsType
from esphome.types import ConfigType

AUTO_LOAD = ["audio", "socket"]
DEPENDENCIES = ["microphone", "network"]
CODEOWNERS = ["@tglenn"]

CONF_PIPELINE = "pipeline"
CONF_SCHEME = "scheme"
CONF_SERVER = "server"

conduit_voice_ns = cg.esphome_ns.namespace("conduit_voice")
ConduitVoice = conduit_voice_ns.class_("ConduitVoice", cg.Component)

StartAction = conduit_voice_ns.class_(
    "StartAction", automation.Action, cg.Parented.template(ConduitVoice)
)
StopAction = conduit_voice_ns.class_(
    "StopAction", automation.Action, cg.Parented.template(ConduitVoice)
)
IsRunningCondition = conduit_voice_ns.class_(
    "IsRunningCondition", automation.Condition, cg.Parented.template(ConduitVoice)
)


def _validate_pipeline(value):
    value = cv.string_strict(value)
    if not value:
        raise cv.Invalid("Pipeline name must not be empty")
    if len(value) > 128:
        raise cv.Invalid("Pipeline name must be 128 characters or fewer")
    if any(not (char.isalnum() or char in "-_") for char in value):
        raise cv.Invalid("Pipeline name may contain only letters, numbers, '-' and '_'")
    return value


CONFIG_SCHEMA = cv.All(
    cv.Schema(
        {
            cv.GenerateID(): cv.declare_id(ConduitVoice),
            cv.Required(CONF_SERVER): cv.string_strict,
            cv.Optional(CONF_SCHEME, default="ws"): cv.one_of("ws", "wss", lower=True),
            cv.Required(CONF_PIPELINE): _validate_pipeline,
            cv.Required(CONF_MICROPHONE): microphone.microphone_source_schema(
                min_bits_per_sample=16,
                max_bits_per_sample=16,
                min_channels=1,
                max_channels=1,
            ),
            cv.Required(CONF_SPEAKER): cv.use_id(speaker.Speaker),
        }
    ).extend(cv.COMPONENT_SCHEMA),
    cv.only_on_esp32,
    socket.consume_sockets(1, "conduit_voice_websocket"),
)

FINAL_VALIDATE_SCHEMA = cv.All(
    cv.Schema(
        {
            cv.Optional(CONF_MICROPHONE): microphone.final_validate_microphone_source_schema(
                "conduit_voice", sample_rate=16000
            ),
        },
        extra=cv.ALLOW_EXTRA,
    ),
)

CONDUIT_VOICE_ACTION_SCHEMA = automation.maybe_simple_id(
    {
        cv.GenerateID(): cv.use_id(ConduitVoice),
    }
)


async def to_code(config: ConfigType) -> None:
    cg.add_global(conduit_voice_ns.using)

    var = cg.new_Pvariable(config[CONF_ID])
    await cg.register_component(var, config)

    mic_source = await microphone.microphone_source_to_code(config[CONF_MICROPHONE])
    cg.add(var.set_microphone_source(mic_source))

    spkr = await cg.get_variable(config[CONF_SPEAKER])
    cg.add(var.set_speaker(spkr))

    cg.add(var.set_server(config[CONF_SERVER]))
    cg.add(var.set_scheme(config[CONF_SCHEME]))
    cg.add(var.set_pipeline(config[CONF_PIPELINE]))

    network.require_high_performance_networking()
    wifi.enable_runtime_power_save_control()
    esp32.add_idf_component(name="espressif/esp_websocket_client", ref="1.5.0")
    esp32.include_builtin_idf_component("esp_http_client")

    cg.add_define("USE_CONDUIT_VOICE", True)


@automation.register_action(
    "conduit_voice.start",
    StartAction,
    CONDUIT_VOICE_ACTION_SCHEMA,
    synchronous=True,
)
async def start_action_to_code(
    config: ConfigType,
    action_id: ID,
    template_arg: cg.TemplateArguments,
    args: TemplateArgsType,
):
    var = cg.new_Pvariable(action_id, template_arg)
    await cg.register_parented(var, config[CONF_ID])
    return var


@automation.register_action(
    "conduit_voice.stop",
    StopAction,
    CONDUIT_VOICE_ACTION_SCHEMA,
    synchronous=True,
)
async def stop_action_to_code(
    config: ConfigType,
    action_id: ID,
    template_arg: cg.TemplateArguments,
    args: TemplateArgsType,
):
    var = cg.new_Pvariable(action_id, template_arg)
    await cg.register_parented(var, config[CONF_ID])
    return var


@automation.register_condition(
    "conduit_voice.is_running",
    IsRunningCondition,
    CONDUIT_VOICE_ACTION_SCHEMA,
)
async def is_running_condition_to_code(
    config: ConfigType,
    condition_id: ID,
    template_arg: cg.TemplateArguments,
    args: TemplateArgsType,
):
    var = cg.new_Pvariable(condition_id, template_arg)
    await cg.register_parented(var, config[CONF_ID])
    return var
