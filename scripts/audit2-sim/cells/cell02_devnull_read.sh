# /dev/null as a read source: immediate EOF
read -r x < /dev/null
echo rc=$?
