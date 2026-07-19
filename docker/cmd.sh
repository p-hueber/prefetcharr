#!/bin/sh

set -e

# Run in compat mode for the old config style
test -z "$PREFETCHARR_CONFIG" -a ! -f /config \
  && exec /prefetcharr \
    --media-server-type "${MEDIA_SERVER_TYPE}" \
    --media-server-url "${MEDIA_SERVER_URL}" \
    --sonarr-url "${SONARR_URL}" \
    --log-dir "${LOG_DIR}" \
    --interval "${INTERVAL:-900}" \
    --remaining-episodes "${REMAINING_EPISODES:-2}" \
    ${USERS:+--users "${USERS}"} \
    ${LIBRARIES:+--libraries "${LIBRARIES}"} \
    --connection-retries 6 

# Write config to /tmp so any userid would work
if test -f /config
then
  # Some users may still bind to /config
  cp /config /tmp/config.toml
else
  sh -c "cat > /tmp/config.toml <<EOF
$PREFETCHARR_CONFIG
EOF"
fi

exec /prefetcharr --config /tmp/config.toml

