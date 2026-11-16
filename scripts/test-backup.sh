#!/usr/bin/env bash
# Tests deploy/backup/autobracket-backup.sh without touching the system: a temporary SQLite
# database, a temporary backup directory, a fake `docker` for the Postgres path. Checks
# permissions (0700/0600), the integrity of the copy, rotation (keep 7), the skipped-Postgres
# path, the Postgres path, and that failures exit non-zero and leave no temporary file.
#
# Needs bash and either the sqlite3 CLI or python3 (whose sqlite3 module then stands in for
# the CLI's few commands the script uses; the real CLI is preferred and used when present).
# Runs shellcheck on the scripts when it is installed.
set -euo pipefail

cd "$(dirname "$0")/.."
script="$PWD/deploy/backup/autobracket-backup.sh"

work="$(mktemp -d "${TMPDIR:-/tmp}/autobracket-backup-test.XXXXXX")"
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/bin"

fail() { echo "FAIL: $*" >&2; exit 1; }
pass() { echo "ok: $*"; }

if command -v shellcheck > /dev/null 2>&1; then
    shellcheck -s bash "$script" "$0"
    pass "shellcheck"
else
    echo "note: shellcheck not installed, skipped"
fi

# --- sqlite3: the real CLI, or a python3 stand-in -------------------------------------------
if command -v sqlite3 > /dev/null 2>&1; then
    SQLITE3=sqlite3
    echo "using $(command -v sqlite3) ($(sqlite3 -version | cut -d' ' -f1))"
else
    command -v python3 > /dev/null 2>&1 || fail "neither sqlite3 nor python3 found"
    SQLITE3="$work/bin/sqlite3"
    # Supports: sqlite3 DB ".backup \"FILE\"" | sqlite3 [-readonly] DB "SQL; SQL;" | -version.
    # Results print one row per line, columns joined by "|", as the CLI does.
    cat > "$SQLITE3" <<'PY'
#!/usr/bin/env python3
import sqlite3, sys
args = sys.argv[1:]
if args and args[0] == "-version":
    print(sqlite3.sqlite_version, "(python shim)"); sys.exit(0)
readonly = False
if args and args[0] == "-readonly":
    readonly = True; args = args[1:]
if len(args) != 2:
    sys.exit("shim: usage: sqlite3 [-readonly] DB COMMAND")
db, cmd = args
try:
    src = sqlite3.connect(f"file:{db}?mode=ro" if readonly else db, uri=readonly, isolation_level=None)
    if cmd.startswith(".backup"):
        dest = cmd[len(".backup"):].strip().strip('"')
        # The CLI's .backup fails on a non-database source; mirror that.
        src.execute("select count(*) from sqlite_master")
        dst = sqlite3.connect(dest)
        src.backup(dst)
        dst.close()
    else:
        for stmt in [s for s in cmd.split(";") if s.strip()]:
            cur = src.execute(stmt)
            for row in cur.fetchall():
                print("|".join("" if v is None else str(v) for v in row))
    src.close()
except sqlite3.Error as e:
    sys.exit(f"Error: {e}")
PY
    chmod 755 "$SQLITE3"
    echo "note: sqlite3 CLI not installed; using a python3 stand-in ($("$SQLITE3" -version))"
fi
export SQLITE3

# --- a WAL-mode database like the server's ------------------------------------------------
db="$work/state/autobracket.sqlite"
mkdir -p "$work/state"
"$SQLITE3" "$db" "PRAGMA journal_mode=WAL; CREATE TABLE events(id INTEGER PRIMARY KEY, name TEXT); INSERT INTO events(name) VALUES ('Nightly'), ('Weekly'), ('Major');" > /dev/null
[ -f "$db" ] || fail "test database not created"

bdir="$work/backups"
envfile="$work/backup.env"
cat > "$envfile" <<EOF
BACKUP_DIR=$bdir
SQLITE_DB=$db
KEEP=7
EOF

run() { # run [extra env assignments...] -> stdout+stderr in $out, status in $status
    set +e
    out="$(env "$@" bash "$script" --env "$envfile" 2>&1)"
    status=$?
    set -e
}

# 1. A missing config file named explicitly is an error.
set +e
bash "$script" --env "$work/nope.env" > /dev/null 2>&1
[ $? -ne 0 ] || fail "missing --env file should fail"
set -e
pass "missing config file fails"

# 2. First run: sqlite copied, postgres skipped, modes right, integrity ok.
run
[ "$status" = 0 ] || fail "first run exited $status: $out"
grep -q "postgres: skipped" <<<"$out" || fail "postgres step should be skipped: $out"
[ "$(stat -c %a "$bdir")" = 700 ] || fail "backup dir mode is $(stat -c %a "$bdir"), want 700"
files=("$bdir"/autobracket-*.sqlite)
[ "${#files[@]}" = 1 ] && [ -f "${files[0]}" ] || fail "expected one sqlite backup, got: ${files[*]}"
[ "$(stat -c %a "${files[0]}")" = 600 ] || fail "backup file mode is $(stat -c %a "${files[0]}"), want 600"
[ "$("$SQLITE3" -readonly "${files[0]}" "PRAGMA integrity_check;")" = ok ] || fail "backup copy fails integrity_check"
[ "$("$SQLITE3" -readonly "${files[0]}" "SELECT count(*) FROM events;")" = 3 ] || fail "backup copy has wrong content"
[ "$("$SQLITE3" -readonly "${files[0]}" "PRAGMA journal_mode;")" = delete ] || fail "backup copy should be journal_mode=delete"
[ -z "$(find "$bdir" -name '.*' -print)" ] || fail "temporary files left behind"
pass "sqlite snapshot: 0700 dir, 0600 file, integrity ok, content matches, postgres skipped"

# The live database is untouched (still WAL, same rows).
[ "$("$SQLITE3" "$db" "PRAGMA journal_mode;")" = wal ] || fail "live database journal mode changed"

# 3. Rotation: eight older files of each kind plus the new one -> exactly 7 remain, newest kept.
for i in 1 2 3 4 5 6 7 8; do
    : > "$bdir/autobracket-2020010$i-000000.sqlite"
    : > "$bdir/zitadel-2020010$i-000000.pgdump"
done
sleep 1 # a new timestamp
run
[ "$status" = 0 ] || fail "second run exited $status: $out"
n=$(find "$bdir" -maxdepth 1 -name 'autobracket-*.sqlite' | wc -l)
[ "$n" = 7 ] || fail "rotation kept $n sqlite files, want 7"
[ ! -e "$bdir/autobracket-20200101-000000.sqlite" ] || fail "oldest file survived rotation"
# 8 fakes + 2 real = 10 files: the 3 oldest go, the 4th oldest is the 7th newest and stays.
[ ! -e "$bdir/autobracket-20200103-000000.sqlite" ] || fail "3rd-oldest file survived rotation"
[ -e "$bdir/autobracket-20200104-000000.sqlite" ] || fail "7th-newest file was deleted"
[ -e "${files[0]}" ] || fail "first run's backup was deleted by rotation"
# Postgres not configured: its old files are not touched by rotation.
[ "$(find "$bdir" -maxdepth 1 -name 'zitadel-*.pgdump' | wc -l)" = 8 ] || fail "pg rotation ran although postgres is not configured"
pass "rotation keeps the newest 7 sqlite snapshots"

# 4. Postgres path through a fake docker that behaves like `docker exec ... pg_dump`.
cat > "$work/bin/docker" <<'SH'
#!/usr/bin/env bash
# Records its arguments, then emits a custom-format-looking dump.
printf '%s\n' "$*" > "${FAKE_DOCKER_LOG:?}"
if [ -n "${FAKE_DOCKER_FAIL:-}" ]; then echo "pg_dump: error: connection refused" >&2; exit 1; fi
printf 'PGDMP\x01\x0e\x00\x04fake dump body\n'
SH
chmod 755 "$work/bin/docker"
cat >> "$envfile" <<EOF
PG_CONTAINER=zitadel-db-1
PG_USER=zitadel
PG_DATABASE=zitadeldb
EOF
sleep 1
run FAKE_DOCKER_LOG="$work/docker.log" PATH="$work/bin:$PATH"
[ "$status" = 0 ] || fail "postgres run exited $status: $out"
grep -q '^exec -i zitadel-db-1 pg_dump -U zitadel --format=custom --no-password zitadeldb$' "$work/docker.log" \
    || fail "unexpected docker invocation: $(cat "$work/docker.log")"
# The real dump: today's timestamp, not one of the 2020 fakes.
pg=("$(find "$bdir" -maxdepth 1 -name 'zitadel-*.pgdump' ! -name 'zitadel-2020*' -print)")
[ -n "${pg[0]}" ] && [ -f "${pg[0]}" ] || fail "no postgres dump written"
[ "$(stat -c %a "${pg[0]}")" = 600 ] || fail "pg dump mode is $(stat -c %a "${pg[0]}"), want 600"
[ "$(head -c 5 "${pg[0]}")" = PGDMP ] || fail "pg dump content wrong"
[ "$(find "$bdir" -maxdepth 1 -name 'zitadel-*.pgdump' | wc -l)" = 7 ] || fail "pg rotation did not keep 7"
[ ! -e "$bdir/zitadel-20200101-000000.pgdump" ] || fail "oldest pg dump survived rotation"
pass "postgres dump via docker exec, 0600, rotation keeps 7"

# 4b. Compose variant.
echo "PG_COMPOSE_FILE=$envfile" >> "$envfile" # any existing file will do for the fake
sleep 1
run FAKE_DOCKER_LOG="$work/docker.log" PATH="$work/bin:$PATH"
[ "$status" = 0 ] || fail "compose run exited $status: $out"
grep -q "^compose -f $envfile exec -T zitadel-db-1 pg_dump " "$work/docker.log" \
    || fail "unexpected compose invocation: $(cat "$work/docker.log")"
pass "postgres dump via docker compose exec -T"

# 5. A failing pg_dump: non-zero exit, no new dump, no temporary file; the sqlite part still ran.
before_pg=$(find "$bdir" -maxdepth 1 -name 'zitadel-*.pgdump' | sort)
sleep 1
run FAKE_DOCKER_LOG="$work/docker.log" FAKE_DOCKER_FAIL=1 PATH="$work/bin:$PATH"
[ "$status" != 0 ] || fail "failing pg_dump should exit non-zero"
grep -q "pg_dump of zitadeldb in zitadel-db-1 failed" <<<"$out" || fail "missing pg error: $out"
[ "$(find "$bdir" -maxdepth 1 -name 'zitadel-*.pgdump' | sort)" = "$before_pg" ] || fail "a dump appeared although pg_dump failed"
[ -z "$(find "$bdir" -name '.*' -print)" ] || fail "temporary files left behind after pg failure"
pass "failing pg_dump exits non-zero and leaves nothing behind"

# 6. A corrupt SQLite database: non-zero exit, no new snapshot, no temporary file.
printf 'this is not a database\n%.0s' {1..200} > "$work/state/corrupt.sqlite"
before_sq=$(find "$bdir" -maxdepth 1 -name 'autobracket-*.sqlite' | sort)
sed -i "s|^SQLITE_DB=.*|SQLITE_DB=$work/state/corrupt.sqlite|; /^PG_/d" "$envfile"
sleep 1
run
[ "$status" != 0 ] || fail "corrupt database should exit non-zero: $out"
[ "$(find "$bdir" -maxdepth 1 -name 'autobracket-*.sqlite' | sort)" = "$before_sq" ] || fail "a snapshot appeared for a corrupt database"
[ -z "$(find "$bdir" -name '.*' -print)" ] || fail "temporary files left behind after sqlite failure"
pass "corrupt database exits non-zero and leaves nothing behind"

# 7. Missing database and bad KEEP are refused.
sed -i "s|^SQLITE_DB=.*|SQLITE_DB=$work/state/missing.sqlite|" "$envfile"
run
[ "$status" != 0 ] || fail "missing database should exit non-zero"
sed -i "s|^SQLITE_DB=.*|SQLITE_DB=$db|; s|^KEEP=.*|KEEP=0|" "$envfile"
run
[ "$status" != 0 ] || fail "KEEP=0 should be refused"
pass "missing database and KEEP=0 are refused"

echo "PASS: all backup checks passed (sqlite3: $SQLITE3)"
