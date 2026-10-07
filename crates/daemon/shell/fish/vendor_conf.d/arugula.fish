# Arugula shell integration for fish. UNTESTED: fish isn't installed on
# the machine this was written on.
#
# arugulad prepends this directory's parent to XDG_DATA_DIRS, so fish loads
# it like any vendor configuration. It reports prompts, commands, exit codes
# (OSC 133, OSC 633) and the directory (OSC 7).

status is-interactive; or exit
set -q __arugula_hooked; and exit
set -g __arugula_hooked 1

function __arugula_prompt --on-event fish_prompt
    if set -q __arugula_status
        printf '\e]133;D;%s\a' $__arugula_status
        set -e __arugula_status
    end
    printf '\e]7;file://%s%s\a\e]133;A\a' (hostname) (string replace -a ' ' '%20' -- (string replace -a '%' '%25' -- $PWD))
end

function __arugula_preexec --on-event fish_preexec
    set -l cmd (string replace -a '\\' '\\\\' -- $argv[1])
    set cmd (string replace -a ';' '\\x3b' -- $cmd)
    printf '\e]633;E;%s\a\e]133;C\a' (string join '\x0a' -- $cmd)
end

function __arugula_postexec --on-event fish_postexec
    set -g __arugula_status $status
end
