R="\033[0m"; B="\033[1;34m"; C="\033[1;36m"; G="\033[1;32m"; Y="\033[1;33m"; RED="\033[1;31m"; DIM="\033[2m"
printf "$B$RED"
printf "  ⚡ niubash — bash, native on Windows\n"
printf "$DIM  one binary, no VM, no /mnt/c, no cmdlet dialect$R\n\n"
sleep 1.2

printf "$Y\$ pwd$R\n"
pwd
sleep 0.7
printf "\n$Y\$ cd \"C:/Program Files\" && pwd$R\n"
cd "C:/Program Files" && pwd
sleep 0.9
printf "\n$Y\$ cd /c/Users/caomengxuan/repo/niubash$R$DIM   # msys-style input also works$R\n"
cd /c/Users/caomengxuan/repo/niubash
pwd
sleep 1.0

printf "\n$Y\$ printf \"%%s\\n\" alpha beta gamma | grep -v gamma$R\n"
printf "%s\n" alpha beta gamma | grep -v gamma
sleep 1.2

printf "\n$Y\$ for i in 1 2 3 4 5; do sum=\$((sum+i)); done; echo \"sum 1..5 = \$sum\"$R\n"
sum=0; for i in 1 2 3 4 5; do sum=$((sum+i)); done; echo "sum 1..5 = $sum"
sleep 1.2

printf "\n$Y\$ cat <<EOF$R\n"
cat <<EOF
native windows paths, unix commands,
bash syntax - pick any two.
we ship all three.
EOF
sleep 1.4

hello() { echo "hello from $1"; }
printf "\n$Y\$ hello() { echo \"hello from \\\$1\"; }; hello niubash$R\n"
hello niubash
sleep 1.2

printf "\n$RED  ── same command line, two shells ─────────────────$R\n"
printf "$DIM  node -e \"console.log(JSON.stringify(process.argv.slice(1)))\" \"a b\" \"\" 'c\"d' \"e\\f\" \"---\"$R\n"
sleep 1.0
printf "$G  niubash :$R  "
node -e "console.log(JSON.stringify(process.argv.slice(1)))" "a b" "" 'c"d' "e\f" "---"
printf "$RED  pwsh    :$R  $DIM[\"a b\",\"cd e\\\\f ---\"]$R   $DIM# empty arg eaten, quote flattened$R\n"
sleep 1.6

printf "\n$Y\$ git status --short$R\n"
git status --short | head -8
sleep 1.2

printf "\n$G  bash syntax, windows paths, real binaries. all three. ⚡$R\n"
sleep 2.0
