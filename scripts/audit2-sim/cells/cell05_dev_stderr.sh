# /dev/stderr as a redirect target (payload lands on stderr)
echo C > /dev/stderr
echo rc=$?
