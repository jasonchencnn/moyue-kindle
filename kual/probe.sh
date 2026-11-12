#!/bin/sh
# Device probe for KUAL: writes facts needed to pick the Rust target.
OUT=/mnt/us/inkread-probe.txt
{
  echo "date: $(date)"
  echo "uname: $(uname -a)"
  grep -m1 'model name\|Hardware' /proc/cpuinfo
  grep -E 'vfp|neon|Features' /proc/cpuinfo | head -1
  grep -E 'MemTotal|MemFree' /proc/meminfo
  echo "fw: $(cat /etc/version.txt 2>/dev/null)"
  ls -l /lib/libc.so* /lib/ld-* 2>/dev/null
  ls /dev/input 2>/dev/null
  command -v lipc-get-prop lipc-set-prop
} > "$OUT" 2>&1
