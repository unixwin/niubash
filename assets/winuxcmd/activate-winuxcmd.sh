#!/usr/bin/env bash

set -e

script_path="$0"
script_dir="${script_path%/*}"
if [ "$script_dir" = "$script_path" ]; then
  script_dir="."
fi

if [ -f "$script_dir/winuxcmd.exe" ] && [ "$(basename "$script_dir")" = "bin" ]; then
  winuxcmd_bin_dir="$script_dir"
  winuxcmd_root="$script_dir/../.."
elif [ -f "$script_dir/winuxcmd.exe" ]; then
  winuxcmd_bin_dir="$script_dir"
  winuxcmd_root="$script_dir"
elif [ -f "./winuxcmd/usr/bin/winuxcmd.exe" ]; then
  winuxcmd_bin_dir="./winuxcmd/usr/bin"
  winuxcmd_root="./winuxcmd"
elif [ -f "./winuxcmd/winuxcmd.exe" ]; then
  winuxcmd_bin_dir="./winuxcmd"
  winuxcmd_root="./winuxcmd"
else
  echo "activate-winuxcmd: winuxcmd.exe not found"
  echo "Run this script from the WinuxCmd usr/bin directory or release root."
  exit 1
fi

winuxcmd_exe="$winuxcmd_bin_dir/winuxcmd.exe"
mode="create"
link_flag=""

case "$1" in
  --remove)
    mode="remove"
    ;;
  --symbolic)
    link_flag="-s"
    ;;
  --help|-h)
    echo "Usage: activate-winuxcmd.sh [--remove] [--symbolic]"
    echo
    echo "Creates command links in usr/bin so ls/cat/grep/etc"
    echo "resolve through PATH when winuxsh starts."
    echo "Modern WinuxCmd builds use WPM for link discovery; older builds"
    echo "fall back to this script's bundled command list."
    echo
    echo "Options:"
    echo "  --remove    Remove generated command links"
    echo "  --symbolic  Create symbolic links instead of hard links"
    exit 0
    ;;
  "")
    ;;
  *)
    echo "activate-winuxcmd: unknown option: $1"
    echo "Try: activate-winuxcmd.sh --help"
    exit 1
    ;;
esac

if [ "$link_flag" = "" ] && "$winuxcmd_exe" wpm version >/dev/null 2>&1; then
  if [ "$mode" = "remove" ]; then
    "$winuxcmd_exe" wpm links remove --root "$winuxcmd_root"
  else
    "$winuxcmd_exe" wpm links rebuild --root "$winuxcmd_root" --force
  fi
  exit $?
fi

commands="
arch b2sum base32 base64 basename basenc cal cat
chcon chgrp chmod chown chroot cksum clear cmp col column comm cp cpio csplit
cut cygpath d2u date dd df diff diff3 dirname
dir dircolors dos2unix du echo env expand expr factor false file
find fmt fold free getconf grep groups head hexdump
hmac256 hostid hostname id infocmp install join jq
kill less link ln locale logname ls lsof man md5sum
mkdir mkfifo mknod mktemp more mpicalc mv nice nl nohup
nproc numfmt od paste patch pathchk pinky pr
printenv printf ps ptx pwd readlink realpath reset
rev rm rmdir runcon sdiff sed seq sha1sum sha224sum
sha256sum sha384sum sha512sum shred shuf sleep sort
split stat stdbuf strings stty sum sync tac tail tee test
[ tic timeout toe top touch tput tr tree true
truncate tsort tty tzset u2d uname unexpand uniq
unix2dos unlink uptime users vdir watch wc which who
whoami xargs xxd yes
"

created=0
removed=0
failed=0

if [ "$mode" != "remove" ]; then
  if [ -f "$winuxcmd_bin_dir/ls.exe" ] && [ -f "$winuxcmd_bin_dir/cat.exe" ] && [ -f "$winuxcmd_bin_dir/grep.exe" ] && [ -f "$winuxcmd_bin_dir/ln.exe" ]; then
    echo "WinuxCmd command links are already active."
    exit 0
  fi
fi

if [ "$mode" = "remove" ]; then
  echo "Removing WinuxCmd command links from $winuxcmd_bin_dir"
  for cmd in $commands; do
    target="$winuxcmd_bin_dir/$cmd.exe"
    if [ -f "$target" ]; then
      "$winuxcmd_exe" rm -f "$target" || failed=$((failed + 1))
      removed=$((removed + 1))
    fi
  done
  echo "Removed: $removed"
else
  echo "Creating WinuxCmd command links in $winuxcmd_bin_dir"
  for cmd in $commands; do
    target="$winuxcmd_bin_dir/$cmd.exe"
    if [ "$link_flag" = "-s" ]; then
      "$winuxcmd_exe" ln -s -f "$winuxcmd_exe" "$target" || failed=$((failed + 1))
    else
      "$winuxcmd_exe" ln -f "$winuxcmd_exe" "$target" || failed=$((failed + 1))
    fi
    created=$((created + 1))
  done
  echo "Created: $created"
fi

if [ "$failed" -ne 0 ]; then
  echo "Failed: $failed"
  exit 1
fi

echo "Done."
