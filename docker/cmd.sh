#!/bin/sh
set -e

# Create config dir if it doesn't exist
mkdir -p /config

# Run in compat mode for the old config style, when no config file is present and PREFETCHARR_CONFIG is unset
test ! -f /config/config.toml && test -z "${PREFETCHARR_CONFIG}" && exec /prefetcharr \
  --media-server-type "${MEDIA_SERVER_TYPE}" \
  --media-server-url "${MEDIA_SERVER_URL}" \
  --sonarr-url "${SONARR_URL}" \
  --log-dir "${LOG_DIR}" \
  --interval "${INTERVAL:-900}" \
  --remaining-episodes "${REMAINING_EPISODES:-2}" \
  ${USERS:+--users "${USERS}"} \
  ${LIBRARIES:+--libraries "${LIBRARIES}"} \
  --connection-retries 6 && exit

# $PREFETCHARR_CONFIG takes precedence over a pre-existing config file
test -n "${PREFETCHARR_CONFIG}" && sh -c 'cat > /config/config.toml <<EOF
${PREFETCHARR_CONFIG}
EOF'

exec /prefetcharr --config /config/config.toml

