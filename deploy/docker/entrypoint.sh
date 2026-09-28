#!/bin/sh
set -eu

exec_unprivileged() {
    if [ "$(id -u)" -eq 0 ]; then
        exec /usr/bin/setpriv \
            --reuid=10001 \
            --regid=10001 \
            --clear-groups \
            --no-new-privs \
            --inh-caps=-all \
            --ambient-caps=-all \
            --bounding-set=-all \
            "$@"
    fi
    exec "$@"
}

exec_worker() {
    if [ "$(id -u)" -eq 0 ]; then
        exec /usr/bin/setpriv \
            --no-new-privs \
            --inh-caps=-setpcap \
            --ambient-caps=-setpcap \
            --bounding-set=-setpcap \
            "$@"
    fi
    exec "$@"
}

case "${1:-}" in
    server)
        shift
        if [ "$(id -u)" -eq 0 ]; then
            mkdir -p /var/lib/ternilo
            chown 10001:10001 /var/lib/ternilo
            # Change permissions as the owner without adding CAP_FOWNER to the launcher.
            /usr/bin/setpriv --reuid=10001 --regid=10001 --clear-groups \
                chmod 700 /var/lib/ternilo
        fi
        exec_unprivileged /usr/local/bin/ternilo-server "$@"
        ;;
    worker)
        shift
        if [ "$(id -u)" -eq 0 ]; then
            mkdir -p /var/lib/ternilo
            chown 0:0 /var/lib/ternilo
            chmod 700 /var/lib/ternilo
        fi
        exec_worker /usr/local/bin/ternilo-worker "$@"
        ;;
    *)
        exec "$@"
        ;;
esac
