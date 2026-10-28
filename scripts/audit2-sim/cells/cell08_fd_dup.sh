# dup stdout onto fd 3 and write through it
exec 3>&1
echo F >&3
exec 3>&-
echo done
