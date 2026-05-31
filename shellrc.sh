# dazai shell integration -- source this from ~/.bashrc or ~/.zshrc.
#
# It couples a heartbeat client to THIS interactive shell. While the shell is
# alive the client holds the daemon's heartbeat connection open; when the shell
# exits, an exit hook kills the client, the connection drops, and the daemon
# (if started with --arm) zeroizes its buffers and SIGKILLs itself.
#
# Prerequisite: start the daemon yourself FIRST, e.g.
#     python3 ~/Documents/dazai/deadman.py --arm --ping-timeout 15 &
# (omit --arm to rehearse safely in dry-run mode first).
#
# Safe to source more than once (idempotent). It does NOT clobber an existing
# EXIT trap: in bash it chains onto it; in zsh it uses the additive zshexit
# hook. It is also safe under `set -u`.

# Where the project lives and which socket to use. Override via env if needed.
: "${DAZAI_DIR:=$HOME/Documents/dazai}"
: "${DEADMAN_SOCK:=${XDG_RUNTIME_DIR:-/tmp}/deadman-$(id -u).sock}"
export DAZAI_DIR DEADMAN_SOCK

# The exit hook: kill the heartbeat (if we still have its PID) so the daemon
# sees the connection drop. set -u safe via the :- default; never errors out.
# Before killing, confirm the PID is still OUR heartbeat -- if the client died
# early and the OS recycled its PID, an unguarded `kill` would signal an
# unrelated process. (A command-line match leaves only a tiny TOCTOU window,
# far narrower than killing a bare stored PID.)
_dazai_atexit() {
    [ -n "${_DAZAI_HB_PID:-}" ] || return 0
    case "$(ps -o command= -p "${_DAZAI_HB_PID}" 2>/dev/null)" in
        *heartbeat.py*) kill "${_DAZAI_HB_PID}" 2>/dev/null || true ;;
    esac
}

dazai_start_heartbeat() {
    # Idempotent: only install once per shell.
    if [ -n "${_DAZAI_INSTALLED:-}" ]; then
        return 0
    fi

    # Interactive shells only.
    case "$-" in
        *i*) ;;
        *) return 0 ;;
    esac

    if ! command -v python3 >/dev/null 2>&1; then
        printf '[dazai] python3 not found; heartbeat NOT started (session UNGUARDED)\n' >&2
        return 0
    fi
    if [ ! -S "$DEADMAN_SOCK" ]; then
        printf '[dazai] daemon socket %s absent; heartbeat NOT started (session UNGUARDED). Start deadman.py first.\n' "$DEADMAN_SOCK" >&2
        return 0
    fi

    python3 "$DAZAI_DIR/heartbeat.py" --socket "$DEADMAN_SOCK" --interval 5 --quiet \
        >/dev/null 2>&1 &
    _DAZAI_HB_PID=$!

    # Register the exit hook WITHOUT clobbering an existing one.
    if [ -n "${ZSH_VERSION:-}" ]; then
        # zsh: add-zsh-hook is additive -- multiple zshexit hooks all run.
        autoload -Uz add-zsh-hook 2>/dev/null && add-zsh-hook zshexit _dazai_atexit
    elif [ -n "${BASH_VERSION:-}" ]; then
        # bash: capture any existing EXIT trap and chain onto it. `trap -p`
        # prints:  trap -- 'CMD' EXIT  (empty if none set).
        _dazai_prev="$(trap -p EXIT)"
        _dazai_prev="${_dazai_prev#trap -- \'}"
        _dazai_prev="${_dazai_prev%\' EXIT}"
        if [ -n "$_dazai_prev" ]; then
            trap "${_dazai_prev}; _dazai_atexit" EXIT
        else
            trap '_dazai_atexit' EXIT
        fi
        unset _dazai_prev
    else
        # Other POSIX shells: single EXIT slot, best effort.
        trap '_dazai_atexit' EXIT
    fi

    _DAZAI_INSTALLED=1
}

dazai_start_heartbeat
