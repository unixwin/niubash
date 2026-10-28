# /dev/full write: ENOSPC surface
printf 'x\n' > /dev/full
echo rc=$?
