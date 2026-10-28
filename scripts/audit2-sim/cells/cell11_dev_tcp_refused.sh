# /dev/tcp open against a port with no listener (discard/9)
exec 3<>/dev/tcp/127.0.0.1/9
echo rc=$?
