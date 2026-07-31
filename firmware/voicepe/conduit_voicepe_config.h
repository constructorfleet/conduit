/*
 * Reference scaffold only. Not compiled into any firmware image; the shipping
 * Voice PE firmware is firmware/esphome/conduit-voicepe.yaml. See README.md.
 */
#ifndef CONDUIT_VOICEPE_CONFIG_H
#define CONDUIT_VOICEPE_CONFIG_H

#include "../common/conduit_converse.h"

#define CONDUIT_VOICEPE_BOARD_ID "voicepe"
#define CONDUIT_VOICEPE_BOARD_NAME "VoicePE"

#ifndef CONDUIT_BOARD_ID
#define CONDUIT_BOARD_ID CONDUIT_VOICEPE_BOARD_ID
#endif

#ifndef CONDUIT_BOARD_NAME
#define CONDUIT_BOARD_NAME CONDUIT_VOICEPE_BOARD_NAME
#endif

#endif
