#!/usr/bin/env bash
# S17: the sprite CLI against geek's wisp (token from ~/.local/share/wisp/token).
SPRITES_API_URL=${SPRITES_API_URL:-http://127.0.0.1:7788}
SPRITE_TOKEN=${SPRITE_TOKEN:-$(cat ~/.local/share/wisp/token)}
export SPRITES_API_URL SPRITE_TOKEN
exec sprite "$@"
