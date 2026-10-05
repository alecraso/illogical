# Run the CLI as the S31 app (adb shell run-as wtf.widgets.illogical.s31 sh /data/local/tmp/il.sh ARGS...)
export HOME=/data/user/0/wtf.widgets.illogical.s31/files
d=$(cat /proc/$(pidof libillogicald.so | cut -d' ' -f1)/cmdline | tr '\0' '\n' | head -1)
exec "$(dirname "$d")/libillogical.so" "$@"
