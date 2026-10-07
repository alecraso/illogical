# Arugula shell integration for zsh. UNTESTED: zsh isn't installed on the
# machine this was written on.
#
# arugulad points ZDOTDIR here; this restores the user's ZDOTDIR, reads
# their .zshenv, and installs precmd/preexec hooks that report prompts,
# commands, exit codes (OSC 133, OSC 633) and the directory (OSC 7). The
# user's .zprofile/.zshrc then load from their own ZDOTDIR as usual.

if [[ -n ${ARUGULA_ZDOTDIR+x} ]]; then
  ZDOTDIR=$ARUGULA_ZDOTDIR
  unset ARUGULA_ZDOTDIR
else
  unset ZDOTDIR
fi
[[ -r ${ZDOTDIR:-$HOME}/.zshenv ]] && source ${ZDOTDIR:-$HOME}/.zshenv

if [[ -o interactive && -z ${__arugula_hooked-} ]]; then
  typeset -g __arugula_hooked=1 __arugula_ran=0
  autoload -Uz add-zsh-hook

  __arugula_precmd() {
    local ret=$?
    if (( __arugula_ran )); then
      print -n "\e]133;D;$ret\a"
      __arugula_ran=0
    fi
    local p=${PWD//\%/%25}
    print -n "\e]7;file://${HOST}${p// /%20}\a\e]133;A\a"
    [[ $PS1 == *'133;B'* ]] || PS1+=$'%{\e]133;B\a%}'
    return $ret
  }

  __arugula_preexec() {
    __arugula_ran=1
    local cmd=${1//\\/\\\\}
    cmd=${cmd//;/\\x3b}
    cmd=${cmd//$'\n'/\\x0a}
    print -rn -- $'\e]633;E;'"$cmd"$'\a\e]133;C\a'
  }

  add-zsh-hook precmd __arugula_precmd
  add-zsh-hook preexec __arugula_preexec
fi
