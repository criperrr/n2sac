#!/bin/sh
set -eu

if [ -z "${FCM_CREDENTIALS_JSON:-}" ] && [ -f "/secrets/firebase-service-account.json" ]; then
  FCM_CREDENTIALS_JSON="$(cat /secrets/firebase-service-account.json)"
  export FCM_CREDENTIALS_JSON
fi

exec /usr/local/bin/nsac-server
