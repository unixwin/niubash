# /dev/stdin inside a piped group
printf 'S\n' | { read -r l < /dev/stdin; echo "got=$l"; }
