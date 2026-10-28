# one-byte read from /dev/zero on a persistent fd (harness timeout guards)
exec 3</dev/zero
read -u 3 -n 1 x
echo rc=$?
exec 3<&-
