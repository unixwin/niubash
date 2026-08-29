printf '[1] vars command-substitution\n'
NAME=niubash
echo "hello:$NAME sub:$(printf inner)"

printf '[2] arithmetic\n'
echo "sum:$((3+5)) pow:$((2**10))"

printf '[3] arrays\n'
ARR=(a b c)
ARR+=(d e)
echo "first:${ARR[0]} all:${ARR[@]} len:${#ARR[@]}"

printf '[4] assoc arrays\n'
declare -A MAP
MAP[key1]=value1
MAP[key2]=value2
echo "key1:${MAP[key1]} keys:${!MAP[@]}"

printf '[5] logic\n'
[ 1 -eq 1 ] && echo eq-true || echo eq-false
[ 1 -eq 2 ] && echo bad || echo neq-false

printf '[6] for list\n'
for i in 1 2 3; do echo "n:$i"; done

printf '[7] for c\n'
for (( i=0; i<3; i++ )); do echo "ci:$i"; done

printf '[8] while\n'
i=0
while [ $i -lt 3 ]; do echo "wi:$i"; ((i++)); done

printf '[9] until\n'
i=3
until [ $i -eq 0 ]; do echo "ui:$i"; ((i--)); done

printf '[10] function\n'
greet() { echo "hello:$1"; }
greet Alice

printf '[11] if\n'
HTTP_CODE=200
if [ $HTTP_CODE -eq 200 ]; then
    echo ok200
elif [ $HTTP_CODE -eq 404 ]; then
    echo notfound
else
    echo other:$HTTP_CODE
fi

printf '[12] case\n'
X=2
case $X in
    1) echo one ;;
    2) echo two ;;
    *) echo other ;;
esac

printf '[13] pipeline\n'
printf 'apple\nbanana\ncherry\n' | grep banana

printf '[14] redirect\n'
echo 'test content' > ./target/niubash-smoke-test.txt
cat ./target/niubash-smoke-test.txt
rm ./target/niubash-smoke-test.txt

printf '[15] special vars\n'
echo "argc:$# args:$* prev:$?"

printf '[16] array slice\n'
A=(1 2 3 4 5)
echo "orig:${A[@]} slice:${A[@]:1:3}"

printf '[17] string ops\n'
S='hello world'
echo "str:$S len:${#S} sub:${S:0:5}"

printf '[18] file tests\n'
[ -d ./target ] && echo target-dir
[ -f ./target/niubash-smoke-test.txt ] || echo temp-missing

printf '[19] export\n'
export MYVAR=123
cmd.exe /C echo child:%MYVAR%

printf '[20] exit status\n'
false && echo bad || echo false-branch
true && echo true-branch
