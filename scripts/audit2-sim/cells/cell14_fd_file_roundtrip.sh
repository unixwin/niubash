# write through a persistent fd to a real file, read it back
exec 3>cell14.out
printf 'I\n' >&3
exec 3>&-
read -r l < cell14.out
echo "got=$l"
