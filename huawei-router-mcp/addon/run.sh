#!/bin/sh
set -eu
# Never echo options or credentials. Never write secrets to disk.
export ROUTER_HOST="$(jq -r '.host' /data/options.json)"
export ROUTER_USERNAME="$(jq -r '.username' /data/options.json)"
export ROUTER_PASSWORD="$(jq -r '.password' /data/options.json)"
if [ "$(jq -r '.discovery' /data/options.json)" = "true" ]; then
  export ROUTER_DISCOVERY=1
else
  export ROUTER_DISCOVERY=0
fi
exec mcp-proxy --pass-environment --host 0.0.0.0 --port 8095 -- python server.py
