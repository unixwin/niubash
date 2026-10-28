# process substitution feeding read
read -r l < <(printf 'P\n')
echo "got=$l"
