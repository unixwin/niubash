# /dev/null opened read-write on a persistent fd
exec 3<>/dev/null
echo rc=$?
exec 3>&-
