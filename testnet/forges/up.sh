#!/usr/bin/env bash
#
# Bring up a real forge for the forge blocks' tests (#93), with two bot
# users and their tokens.
#
#   testnet/forges/up.sh forgejo     Forgejo, ready in seconds
#   testnet/forges/up.sh gitlab      GitLab CE and a shell runner: minutes
#
# Writes testnet/forges/.state/<forge>.json: the forge's URL, the URL it
# reaches the host by (for webhooks), and each bot's login and token.
# crates/daemon/tests/integration/forges_real.rs reads it (ARUGULA_TESTNET_FORGES
# names the directory). The tokens are the stack's own, made fresh by this
# script; nothing here is a real account.
#
# Re-running is safe: users that exist are kept and given new tokens.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
FORGE="${1:-forgejo}"
STATE="$HERE/.state"
COMPOSE=(docker compose -f "$HERE/compose.yaml" --profile "$FORGE")
BOTS="illo-author illo-reviewer"

log() { echo "[forges up $FORGE] $*" >&2; }
die() { log "FAIL: $*"; exit 1; }

# These tests need Docker: without it they fail, unless ARUGULA_SKIP_DOCKER=1
# asks to skip them, which says loudly that nothing ran.
if ! command -v docker >/dev/null 2>&1 || ! docker info >/dev/null 2>&1; then
  if [ "${ARUGULA_SKIP_DOCKER:-}" = 1 ]; then
    echo "!!! ARUGULA_SKIP_DOCKER=1 and no Docker: NOTHING RAN (testnet/forges) !!!" >&2
    exit 0
  fi
  echo "FAIL: Docker is not available, and testnet/forges needs it (ARUGULA_SKIP_DOCKER=1 skips, running nothing)" >&2
  exit 1
fi
mkdir -p "$STATE"
chmod 700 "$STATE"

up_forgejo() {
  local port="${ARUGULA_TESTNET_FORGEJO_PORT:-17746}" u tok users=""
  "${COMPOSE[@]}" up -d --wait >&2
  fj() { "${COMPOSE[@]}" exec -T -u git forgejo forgejo "$@"; }
  for u in illo-admin $BOTS; do
    if ! fj admin user list 2>/dev/null | awk '{print $2}' | grep -qx "$u"; then
      local admin=""; [ "$u" = illo-admin ] && admin="--admin"
      # shellcheck disable=SC2086
      fj admin user create --username "$u" --email "$u@example.test" --password "pw-$(openssl rand -hex 12)" \
        --must-change-password=false $admin >/dev/null
    fi
    tok="$(fj admin user generate-access-token --username "$u" --token-name "t$(date +%s)-$RANDOM" --scopes all --raw | tr -d '\r\n')"
    [ -n "$tok" ] || die "no token for $u"
    users="$users${users:+,}\"$u\":{\"token\":\"$tok\"}"
  done
  umask 077
  cat > "$STATE/forgejo.json" <<JSON
{"url":"http://127.0.0.1:$port","hook_host":"host.docker.internal","users":{$users}}
JSON
  log "up: http://127.0.0.1:$port ($STATE/forgejo.json)"
}

# GitLab's Rails console is slow to start (30 s or so), so every user, token
# and setting is made in one run.
up_gitlab() {
  local port="${ARUGULA_TESTNET_GITLAB_PORT:-17747}" out
  log "starting GitLab CE (a few minutes the first time)"
  [ -s "$STATE/gitlab-root-password" ] || (umask 077; openssl rand -hex 16 > "$STATE/gitlab-root-password")
  ARUGULA_TESTNET_GITLAB_ROOT_PASSWORD="Ig-$(cat "$STATE/gitlab-root-password")"
  export ARUGULA_TESTNET_GITLAB_ROOT_PASSWORD
  "${COMPOSE[@]}" up -d --wait >&2
  out="$("${COMPOSE[@]}" exec -T gitlab gitlab-rails runner - <<'RUBY'
s = ApplicationSetting.current
# Hooks to the host only: GitLab's counterpart of Forgejo's ALLOWED_HOST_LIST.
s.update!(allow_local_requests_from_web_hooks_and_services: false,
          outbound_local_requests_allowlist_raw: 'host.docker.internal',
          signup_enabled: false)
org = Organizations::Organization.default_organization
root = User.find_by_username('root')
out = {}
%w[illo-author illo-reviewer].each do |name|
  u = User.find_by_username(name)
  unless u
    u = Users::CreateService.new(root, username: name, name: name, email: "#{name}@example.test",
      password: SecureRandom.hex(16) + 'Aa1!', skip_confirmation: true,
      organization_id: org&.id).execute.payload[:user]
    u.save! if u.respond_to?(:save!)
  end
  t = u.personal_access_tokens.create!(name: "t#{Time.now.to_i}", scopes: %w[api read_user write_repository],
        expires_at: 30.days.from_now)
  out[name] = { token: t.token }
end
rt = root.personal_access_tokens.create!(name: "t#{Time.now.to_i}", scopes: %w[api create_runner], expires_at: 30.days.from_now)
out['root'] = { token: rt.token }
puts "ARUGULA-USERS #{out.to_json}"
RUBY
)"
  local users
  users="$(printf '%s\n' "$out" | sed -n 's/^ARUGULA-USERS //p')"
  [ -n "$users" ] || { printf '%s\n' "$out" >&2; die "the Rails runner made no users"; }
  register_runner "$port" "$(printf '%s' "$users" | python3 -c 'import json,sys; print(json.load(sys.stdin)["root"]["token"])')"
  umask 077
  cat > "$STATE/gitlab.json" <<JSON
{"url":"http://127.0.0.1:$port","hook_host":"host.docker.internal","users":$users}
JSON
  log "up: http://127.0.0.1:$port ($STATE/gitlab.json)"
}

# An instance runner with the shell executor, inside the runner container:
# enough for a job that fails and a retry that runs it again.
register_runner() {
  local port="$1" root="$2" tok
  if "${COMPOSE[@]}" exec -T gitlab-runner test -s /etc/gitlab-runner/config.toml; then
    "${COMPOSE[@]}" exec -d gitlab-runner gitlab-runner run >&2
    return
  fi
  tok="$(curl -fsS -X POST -H "PRIVATE-TOKEN: $root" "http://127.0.0.1:$port/api/v4/user/runners" \
    -d runner_type=instance_type -d run_untagged=true -d description=arugula-testnet \
    | python3 -c 'import json,sys; print(json.load(sys.stdin)["token"])')"
  "${COMPOSE[@]}" exec -T gitlab-runner gitlab-runner register --non-interactive \
    --url "http://gitlab:$port" --clone-url "http://gitlab:$port" --token "$tok" --executor shell >&2
  "${COMPOSE[@]}" exec -d gitlab-runner gitlab-runner run >&2
}

case "$FORGE" in
  forgejo) up_forgejo ;;
  gitlab) up_gitlab ;;
  *) echo "usage: testnet/forges/up.sh forgejo|gitlab" >&2; exit 2 ;;
esac
