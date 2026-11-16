#!/usr/bin/env bash
# Nightly LOCAL backup of the autobracket SQLite database and, when configured, the Zitadel
# Postgres database (running in a Docker container). Run by autobracket-backup.timer as root.
#
#   - a consistent online snapshot of the SQLite database with `sqlite3 DB ".backup FILE"`
#     (safe while the server runs; a plain copy of a WAL-mode database is not), verified with
#     `PRAGMA integrity_check`;
#   - `pg_dump --format=custom` of the Zitadel database through `docker exec` (or
#     `docker compose exec -T`), only when PG_CONTAINER is set; skipped otherwise;
#   - everything is written to a temporary file in BACKUP_DIR and renamed into place only
#     after it verified, so a half-written file never carries a final name;
#   - BACKUP_DIR is mode 0700 and every file 0600 (root only); the newest KEEP of each kind
#     are kept, older ones deleted.
#
# There is NO off-site copy: this protects against bad writes, botched migrations and
# accidental deletion, not against losing the disk or the machine. See docs/backup.md.
#
# Configuration: /etc/autobracket/backup.env (KEY=value lines, sourced by this shell; see
# deploy/backup/backup.env.example), or the file named by --env FILE / $AUTOBRACKET_BACKUP_ENV.
# Any failure exits non-zero (and the unit fails, visible in `systemctl --failed`).
set -euo pipefail

usage() {
    echo "usage: autobracket-backup.sh [--env FILE]" >&2
}

env_file="${AUTOBRACKET_BACKUP_ENV:-/etc/autobracket/backup.env}"
env_file_given=0
[ -n "${AUTOBRACKET_BACKUP_ENV:-}" ] && env_file_given=1
while [ $# -gt 0 ]; do
    case "$1" in
        --env)
            [ $# -ge 2 ] || { usage; exit 2; }
            env_file="$2"
            env_file_given=1
            shift 2
            ;;
        -h | --help) usage; exit 0 ;;
        *) usage; exit 2 ;;
    esac
done

log() { printf '%s autobracket-backup: %s\n' "$(date '+%Y-%m-%dT%H:%M:%S%z')" "$*"; }
die() { log "error: $*" >&2; exit 1; }

if [ -f "$env_file" ]; then
    # shellcheck disable=SC1090
    . "$env_file"
elif [ "$env_file_given" = 1 ]; then
    die "config file $env_file not found"
else
    log "no $env_file; using defaults"
fi

# --- Settings and their defaults (documented in backup.env.example) ---------------------
BACKUP_DIR="${BACKUP_DIR:-/var/backups/autobracket}"
SQLITE_DB="${SQLITE_DB:-/var/lib/autobracket/autobracket.sqlite}"
SQLITE3="${SQLITE3:-sqlite3}"
KEEP="${KEEP:-7}"
PG_CONTAINER="${PG_CONTAINER:-}"
PG_COMPOSE_FILE="${PG_COMPOSE_FILE:-}"
PG_USER="${PG_USER:-postgres}"
PG_DATABASE="${PG_DATABASE:-zitadel}"
DOCKER="${DOCKER:-docker}"

case "$KEEP" in
    '' | *[!0-9]*) die "KEEP must be a positive integer, got '$KEEP'" ;;
esac
KEEP=$((10#$KEEP)) # "08" is eight, not octal
[ "$KEEP" -ge 1 ] || die "KEEP must be at least 1"
case "$BACKUP_DIR" in
    /?*) ;;
    *) die "BACKUP_DIR must be an absolute path, got '$BACKUP_DIR'" ;;
esac
case "$BACKUP_DIR$SQLITE_DB" in
    *\'* | *\"*) die "paths may not contain quotes" ;;
esac

# Root-only directory and files, from the first byte written.
umask 077
mkdir -p "$BACKUP_DIR"
chmod 700 "$BACKUP_DIR"

stamp="$(date +%Y%m%d-%H%M%S)"
tmp_files=()
cleanup() {
    local f
    for f in "${tmp_files[@]+"${tmp_files[@]}"}"; do
        rm -f -- "$f" "$f-journal" "$f-wal" "$f-shm"
    done
}
trap cleanup EXIT

# rotate PREFIX SUFFIX JUST_WRITTEN: deletes all but the newest $KEEP files named
# PREFIX<YYYYMMDD-HHMMSS>SUFFIX in BACKUP_DIR (the stamp sorts chronologically). Only names of
# exactly that shape are touched, so a hand-placed file is left alone (and not counted), and
# the file this run just wrote is never deleted, even when the clock stepped backwards since
# the last run. Temporary files start with a dot and never match.
rotate() {
    local prefix="$1" suffix="$2" keep_name suffix_re n=0 f
    keep_name="$(basename "$3")"
    suffix_re="${suffix//./\\.}" # a literal dot in the regex below
    # The just-written file plus, newest first, the KEEP-1 newest others stay: `tail -n +KEEP`
    # of the others (which exclude it) names the rest.
    while IFS= read -r f; do
        [ -n "$f" ] || continue
        rm -f -- "$f"
        n=$((n + 1))
    done < <(find "$BACKUP_DIR" -maxdepth 1 -type f -regextype posix-extended \
        -regex ".*/${prefix}[0-9]{8}-[0-9]{6}${suffix_re}" ! -name "$keep_name" -print \
        | sort -r | tail -n +"$KEEP")
    [ "$n" = 0 ] || log "removed $n old $prefix*$suffix (keeping $KEEP)"
}

# --- 1. SQLite ----------------------------------------------------------------------------
[ -f "$SQLITE_DB" ] || die "SQLite database $SQLITE_DB not found"
command -v "$SQLITE3" > /dev/null 2>&1 || die "$SQLITE3 not found (install the sqlite3 CLI or set SQLITE3)"

sqlite_final="$BACKUP_DIR/autobracket-$stamp.sqlite"
sqlite_tmp="$BACKUP_DIR/.autobracket-$stamp.sqlite.tmp"
tmp_files+=("$sqlite_tmp")
rm -f -- "$sqlite_tmp"

# The online backup API copies a consistent snapshot while the server keeps writing.
"$SQLITE3" "$SQLITE_DB" ".backup \"$sqlite_tmp\"" || die "sqlite3 .backup of $SQLITE_DB failed"
[ -s "$sqlite_tmp" ] || die "sqlite3 .backup wrote an empty file"

# The copy inherits WAL mode; switch it to a self-contained single file (no -wal/-shm needed
# to open it later), then verify. Expected output: "delete" then "ok".
check="$("$SQLITE3" "$sqlite_tmp" "PRAGMA journal_mode=DELETE; PRAGMA integrity_check;")" \
    || die "verifying $sqlite_tmp failed"
case "$check" in
    "delete"$'\n'"ok") ;;
    *) die "integrity_check of the SQLite copy failed: ${check:-<no output>}" ;;
esac
chmod 600 "$sqlite_tmp"
mv -f -- "$sqlite_tmp" "$sqlite_final"
log "sqlite: $sqlite_final ($(stat -c %s "$sqlite_final") bytes, integrity ok)"
rotate autobracket- .sqlite "$sqlite_final"

# --- 2. Zitadel Postgres (optional) -----------------------------------------------------
if [ -z "$PG_CONTAINER" ]; then
    log "postgres: skipped (PG_CONTAINER not set)"
else
    command -v "$DOCKER" > /dev/null 2>&1 || die "$DOCKER not found (set DOCKER)"
    pg_final="$BACKUP_DIR/zitadel-$stamp.pgdump"
    pg_tmp="$BACKUP_DIR/.zitadel-$stamp.pgdump.tmp"
    tmp_files+=("$pg_tmp")
    rm -f -- "$pg_tmp"

    # pg_dump runs inside the container (the host needs no Postgres client); its custom
    # format is compressed and restored with pg_restore (docs/backup.md).
    if [ -n "$PG_COMPOSE_FILE" ]; then
        [ -f "$PG_COMPOSE_FILE" ] || die "PG_COMPOSE_FILE $PG_COMPOSE_FILE not found"
        exec_cmd=("$DOCKER" compose -f "$PG_COMPOSE_FILE" exec -T "$PG_CONTAINER")
    else
        exec_cmd=("$DOCKER" exec -i "$PG_CONTAINER")
    fi
    "${exec_cmd[@]}" pg_dump -U "$PG_USER" --format=custom --no-password "$PG_DATABASE" \
        > "$pg_tmp" < /dev/null || die "pg_dump of $PG_DATABASE in $PG_CONTAINER failed"
    [ -s "$pg_tmp" ] || die "pg_dump wrote an empty file"
    # Custom-format dumps start with the magic "PGDMP".
    [ "$(head -c 5 "$pg_tmp")" = "PGDMP" ] || die "pg_dump output is not a custom-format dump"
    chmod 600 "$pg_tmp"
    mv -f -- "$pg_tmp" "$pg_final"
    log "postgres: $pg_final ($(stat -c %s "$pg_final") bytes)"
    rotate zitadel- .pgdump "$pg_final"
fi

log "done; $BACKUP_DIR holds $(find "$BACKUP_DIR" -maxdepth 1 -type f ! -name '.*' | wc -l) files (local only, no off-site copy)"
