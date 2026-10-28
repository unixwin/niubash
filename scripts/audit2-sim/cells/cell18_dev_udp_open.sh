# /dev/udp open (no handshake: succeeds silently where supported)
exec 3<>/dev/udp/127.0.0.1/9
echo rc=$?
exec 3>&-
