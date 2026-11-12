# torq-agent-run [--share-fd <n> <dir>] [--] [<command> …] — started by `torq agent run`, through
# sudo, as root: it does one thing as root, then is the agent for good.
#
# As root, in a mount namespace of its own: every home hidden, and the one directory shared with the
# agent — the directory torqcli opened, as the person, handed over as descriptor <n> — bound where it
# is — by its name, then checked against that descriptor, device and inode (its name could have been
# pointed elsewhere; then nothing runs). Then it drops to
# torq-agent (setpriv: its uid and group, no capabilities, no new privileges) and runs the program in
# bubblewrap, in a process namespace of its own (nothing it starts outlives it): the people's runtime
# directories (their sockets: ssh-agent, D-Bus) hidden, /tmp its own,
# its home the agent's. Nothing the person asked for runs as root; the agent has no way into the home.
set -euo pipefail
if [ "$(id -u)" != 0 ]; then
  echo "torq-agent-run: is started by torq agent run (through sudo)" >&2
  exit 2
fi
share="" fd=""
if [ "${1-}" = --share-fd ]; then
  fd=${2-} share=${3-}
  shift 3
fi
if [ "${1-}" = -- ]; then
  shift
fi
if [ $# -eq 0 ]; then
  set -- bash
fi
if [ -n "$share" ]; then
  person=${SUDO_UID:?torq-agent-run: shares only for the person who ran sudo}
  case "$fd" in
    '' | *[!0-9]*) echo "torq-agent-run: --share-fd needs a descriptor number" >&2; exit 2 ;;
  esac
  link=/proc/self/fd/$fd
  # The open directory itself: a directory, the person's, and where it says it is.
  if [ ! -d "$link" ] || [ "$(stat -L -c %u "$link")" != "$person" ] || [ "$(readlink "$link")" != "$share" ]; then
    echo "torq-agent-run: descriptor $fd is not $share, a directory of yours" >&2
    exit 2
  fi
  case "$share" in
    /home/*/*) ;;
    *) echo "torq-agent-run: shares a directory inside a home, not $share" >&2; exit 2 ;;
  esac
fi
# (The inner script is single-quoted on purpose: its variables are its own arguments.)
# shellcheck disable=SC2016
# The shared directory bound, as root, and checked to be the one that was opened (a descriptor from
# another mount namespace cannot itself be bound); every home hidden around it; then the agent.
exec unshare --mount --propagation private -- bash -euo pipefail -c '
  share=$1 fd=$2
  shift 2
  if [ -n "$share" ]; then
    # Bound by its name, on a staging point only root reaches; then checked to be the very directory
    # that was opened (device and inode): if a link on the way led elsewhere, nothing runs.
    stage=/run/torq-agent-stage
    mount --bind -- "$share" "$stage"
    # No set-id program runs from it, and no device node works, while the agent is in it. That holds in
    # the namespace of the agent only: the view the person has of the folder has no such mount, so what it
    # leaves set-id is made plain when it ends (below; the audit of M3, L2).
    mount -o remount,bind,nosuid,nodev -- "$stage"
    if [ "$(stat -c %d:%i "$stage")" != "$(stat -L -c %d:%i "/proc/self/fd/$fd")" ]; then
      echo "torq-agent-run: $share is not the directory that was opened (a link changed?): nothing run" >&2
      exit 2
    fi
    eval "exec $fd<&-"
  fi
  mount -t tmpfs -o mode=0755,nosuid,nodev torq-agent-homes /home
  if [ -n "$share" ]; then
    mkdir -p -- "$share"
    mount --move "$stage" "$share"
  fi
  rc=0
  setpriv --reuid torq-agent --regid torq-agent --init-groups --no-new-privs \
    --inh-caps=-all --bounding-set=-all -- \
    bwrap --die-with-parent --unshare-pid --dev-bind / / --proc /proc \
      --tmpfs /run/user --tmpfs /tmp --tmpfs /root \
      --clearenv \
      --setenv HOME /var/lib/torq-agent --setenv USER torq-agent --setenv LOGNAME torq-agent \
      --setenv SHELL /run/current-system/sw/bin/bash \
      --setenv PATH /run/wrappers/bin:/run/current-system/sw/bin \
      --setenv TERM "${TERM:-dumb}" \
      --chdir "${share:-/var/lib/torq-agent}" \
      -- "$@" || rc=$?
  # A set-id file the agent left in the folder would run as the agent outside the sandbox, later: its bits
  # are taken off. As the agent, not as root: the files are owned by the agent, and an owner may change
  # them. The folder belongs to the person, who could swap a folder in it for a link while this runs,
  # after find looks at a file and before chmod, which goes by the path again. As root, chmod would then
  # take the bits off whatever the link led to (the wrapper of sudo, say). As the agent, a link can lead
  # it only to what the agent could change anyway (the review of 2026-11-11, KNOWN-GAPS 6zz).
  if [ -n "$share" ]; then
    found=$(setpriv --reuid torq-agent --regid torq-agent --init-groups --no-new-privs \
      --inh-caps=-all --bounding-set=-all -- \
      find "$share" -xdev -user torq-agent -perm /6000 -type f -print -exec chmod ug-s {} + 2>/dev/null || true)
    [ -z "$found" ] || echo "torq agent run: set-id bits taken off what the agent left: $found" >&2
  fi
  exit "$rc"
' torq-agent-run "$share" "$fd" "$@"
