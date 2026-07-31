/*
 * Reference scaffold only. Not compiled into any firmware image; the shipping
 * Satellite1 firmware is firmware/esphome/conduit-sat1.yaml. See README.md.
 */
#ifndef CONDUIT_SAT1_CONFIG_H
#define CONDUIT_SAT1_CONFIG_H

#include "../common/conduit_converse.h"

#define CONDUIT_SAT1_BOARD_ID "sat1"
#define CONDUIT_SAT1_BOARD_NAME "Satellite1"

#ifndef CONDUIT_BOARD_ID
#define CONDUIT_BOARD_ID CONDUIT_SAT1_BOARD_ID
#endif

#ifndef CONDUIT_BOARD_NAME
#define CONDUIT_BOARD_NAME CONDUIT_SAT1_BOARD_NAME
#endif

#endif
