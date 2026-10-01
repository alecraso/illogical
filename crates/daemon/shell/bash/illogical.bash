# illogical shell integration for bash (4.4+).
#
# illogicald starts bash with --posix and ENV pointing here, the way Ghostty
# injects its integration: in POSIX mode an interactive bash reads only $ENV,
# so this file turns POSIX mode off again, reads the startup files bash would
# have read, and then installs the hooks.
#
# The hooks tell the terminal (OSC 133, plus VS Code's OSC 633 for the
# command line) where prompts and commands begin and end, the exit code, and
# the working directory (OSC 7). Nothing here changes how the shell behaves.

# On a machine (a VM pane) this file arrives in $ILLOGICAL_BASH_SCRIPT and
# ENV writes it to a temporary file; tidy both away.
if [[ -n "${ILLOGICAL_BASH_SCRIPT:-}" ]]; then
  builtin unset ILLOGICAL_BASH_SCRIPT
  command rm -f -- "${BASH_SOURCE[0]}"
fi

if [[ -n "${ILLOGICAL_BASH_INJECT:-}" ]]; then
  builtin unset ENV ILLOGICAL_BASH_INJECT
  builtin set +o posix
  if [[ -n "${ILLOGICAL_BASH_LOGIN:-}" ]]; then
    if [[ -z "${ILLOGICAL_BASH_NOPROFILE:-}" ]]; then
      [[ -r /etc/profile ]] && builtin source /etc/profile
      for __illogical_f in ~/.bash_profile ~/.bash_login ~/.profile; do
        if [[ -r "$__illogical_f" ]]; then
          builtin source "$__illogical_f"
          break
        fi
      done
      builtin unset __illogical_f
    fi
  elif [[ -z "${ILLOGICAL_BASH_NORC:-}" ]]; then
    [[ -r /etc/bash.bashrc ]] && builtin source /etc/bash.bashrc
    [[ -r ~/.bashrc ]] && builtin source ~/.bashrc
  fi
  builtin unset ILLOGICAL_BASH_LOGIN ILLOGICAL_BASH_NOPROFILE ILLOGICAL_BASH_NORC
fi

if [[ -z "${__illogical_hooked:-}" && $- == *i* ]]; then
  __illogical_hooked=1
  __illogical_first=1
  __illogical_histnum=

  # The command line, escaped so it fits in an OSC: `\\` for a backslash,
  # `\xHH` for ';' and control characters.
  __illogical_escape() {
    local s=$1 out= c i
    for ((i = 0; i < ${#s}; i++)); do
      c=${s:i:1}
      case $c in
        '\') out+='\\' ;;
        ';') out+='\x3b' ;;
        [[:cntrl:]]) printf -v c '\\x%02x' "'$c"; out+=$c ;;
        *) out+=$c ;;
      esac
    done
    printf '%s' "$out"
  }

  __illogical_histnum() {
    local h
    h=$(HISTTIMEFORMAT= builtin history 1)
    h=${h#"${h%%[! ]*}"}
    printf '%s' "${h%%[!0-9]*}"
  }

  # Before each prompt: the last command's exit code, the directory, and
  # "a prompt starts here". Runs first, and hands $? on unchanged.
  __illogical_precmd() {
    local ret=$?
    if [[ -n $__illogical_first ]]; then
      __illogical_first=
    else
      builtin printf '\e]133;D;%s\a' "$ret"
    fi
    local path=${PWD//%/%25}
    path=${path// /%20}
    builtin printf '\e]7;file://%s%s\a\e]133;A\a' "${HOSTNAME:-localhost}" "$path"
    __illogical_histnum=$(__illogical_histnum)
    return $ret
  }

  # After the prompt is drawn: mark where the input starts. Runs last, so it
  # sees the PS1 that other prompt hooks (starship, etc.) built.
  __illogical_ps1() {
    local ret=$?
    [[ $PS1 == *'133;B'* ]] || PS1+='\[\e]133;B\a\]'
    return $ret
  }

  # Expanded in PS0 (a subshell) just before a command runs: the command
  # line, if it reached history, then "output starts here".
  __illogical_preexec() {
    local h n cmd=
    h=$(HISTTIMEFORMAT= builtin history 1)
    h=${h#"${h%%[! ]*}"}
    n=${h%%[!0-9]*}
    if [[ -n $n && $n != "$__illogical_histnum" ]]; then
      cmd=${h#"$n"}
      cmd=${cmd#"${cmd%%[! ]*}"}
    fi
    builtin printf '\e]633;E;%s\a\e]133;C\a' "$(__illogical_escape "$cmd")"
  }

  if [[ "$(declare -p PROMPT_COMMAND 2>/dev/null)" == "declare -a"* ]]; then
    PROMPT_COMMAND=(__illogical_precmd "${PROMPT_COMMAND[@]}" __illogical_ps1)
  else
    PROMPT_COMMAND="__illogical_precmd${PROMPT_COMMAND:+; $PROMPT_COMMAND}; __illogical_ps1"
  fi
  PS0='$(__illogical_preexec)'"${PS0:-}"
fi
