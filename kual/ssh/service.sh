#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# Run from /var/local/moyue-ssh (upstart changed there first) and exec the
# extension's private copy of KOReader's Dropbear. Paths below are relative on
# purpose: a later rename or replacement of the directory cannot change what
# this process runs or reads.
#
# -F foreground, -E log to stderr, -R create missing host keys, -s keys only,
# -p 22 all available interfaces. The firewall opens only wlan0.
set -u
PATH=/usr/sbin:/sbin:$PATH
export PATH
umask 077

R=${AGENT_TEST_ROOT:-}
JOB=moyue-ssh
OWNER=${AGENT_TEST_OWNER:-0}
EXPECT=${AGENT_TEST_DIR_MODE:-000}
RULE='INPUT -i wlan0 -p tcp --dport 22 -m state --state NEW,ESTABLISHED -j ACCEPT'
LOG=log

log() {
    printf '%s %s\n' "$(date '+%Y-%m-%d %H:%M:%S')" "$*" >> "$LOG" 2> /dev/null || :
    logger -t "$JOB" "$*" 2> /dev/null || :
}

refuse() {
    log "not starting: $*"
    printf 'moyue-ssh: %s\n' "$*" >&2
    exit 1
}

fw_present() {
    # Word splitting is intentional: RULE is the exact iptables argument list.
    iptables -C $RULE 2> /dev/null
}

port_state() {
    seen=0
    busy=0
    for f in "$R/proc/net/tcp" "$R/proc/net/tcp6"; do
        [ -r "$f" ] || continue
        seen=1
        if awk '$2 ~ /:0016$/ && $4 == "0A" { found = 1 } END { exit !found }' "$f"; then
            busy=1
        fi
    done
    if [ "$seen" = 0 ]; then
        echo unknown
    elif [ "$busy" = 1 ]; then
        echo busy
    else
        echo free
    fi
}

firewall_add() {
    fw_present && return 0
    # Record ownership before adding, so an interrupted start still gets its
    # exact rule removed by post-stop.
    : > iptables-added || return 1
    chmod 600 iptables-added 2> /dev/null || :
    if ! iptables -A $RULE; then
        rm -f iptables-added
        return 1
    fi
    return 0
}

firewall_remove() {
    [ -f iptables-added ] || return 0
    # Remove one exact rule only; other rules (including the 2222 one) stay.
    iptables -D $RULE 2> /dev/null || :
    rm -f iptables-added
}

action=${1:-start}
case "$action" in
start | '')
    set -- $(stat -c '%u %a' . 2> /dev/null)
    [ $# -eq 2 ] || refuse "cannot stat the state directory"
    [ "$1" = "$OWNER" ] || refuse "state directory owner $1 is not $OWNER"
    [ "$((0$2))" -eq "$((0$EXPECT))" ] || refuse "state directory mode $2 is not $EXPECT"
    [ -L ./service.sh ] && refuse "./service.sh is a symlink"
    [ -f ./service.sh ] || refuse "./service.sh is missing"
    [ -L ./dropbear ] && refuse "./dropbear is a symlink"
    [ -f ./dropbear ] && [ -x ./dropbear ] || refuse "./dropbear is missing or not executable"
    [ -L ./settings ] && refuse "./settings is a symlink"
    [ -d ./settings ] || refuse "./settings is missing"
    [ -L ./settings/SSH ] && refuse "./settings/SSH is a symlink"
    [ -d ./settings/SSH ] || refuse "./settings/SSH is missing"
    [ -L ./settings/SSH/authorized_keys ] && refuse "authorized_keys is a symlink"
    [ -f ./settings/SSH/authorized_keys ] && [ -s ./settings/SSH/authorized_keys ] ||
        refuse "authorized_keys is missing or empty"
    for k in ./settings/SSH/dropbear_*_host_key; do
        [ -L "$k" ] && refuse "$k is a symlink"
    done

    case $(port_state) in
    busy) refuse "port 22 is already listening; not stopping another service" ;;
    unknown) refuse "cannot read /proc/net/tcp to check port 22" ;;
    esac
    firewall_add || refuse "cannot add the wlan0 TCP 22 firewall rule"
    log "starting dropbear (-F -E -R -s -p 22)"
    exec ./dropbear -F -E -R -s -p 22
    ;;
cleanup)
    firewall_remove
    exit 0
    ;;
*)
    printf 'usage: service.sh [start|cleanup]\n' >&2
    exit 2
    ;;
esac
