# Nightly local backup

`deploy/backup/` holds a nightly backup for a systemd deployment: a consistent online snapshot
of the autobracket SQLite database and, when configured, a `pg_dump` of the Zitadel Postgres
database running in a Docker container. Both land in a root-only directory on the **same
machine**.

## What it protects against, and what it does not

**There is no off-site copy.** Nothing in this kit copies the backups to another machine,
disk or service; `/var/backups/autobracket` lives on the same disk as the live database.

| Against | Covered? |
| --- | --- |
| A bad write, a mistaken `admin` command, a deleted event or player | yes: restore last night's file |
| A botched migration or a server upgrade that corrupts the database | yes: the snapshots were taken before it |
| A broken live database (`PRAGMA integrity_check` fails after a crash) | yes |
| Losing the disk, the VM, or the whole machine | **no** |
| Ransomware or an attacker with root (they can delete the backups too) | **no** |
| Anything older than the last 7 nights | **no** (`KEEP` sets the count) |

If any of the "no" rows matter to you, copy `/var/backups/autobracket` somewhere else
yourself (e.g. `rsync` to another host from a cron job); the files are self-contained and safe
to copy at any moment, since each one is renamed into place only after it was verified.

Not covered at all: uploaded replays (`replay_dir`, large and reproducible from the players'
own files), the organiser key (it is not on the server: keep your own copy, README "Backups"),
and the config in `/etc/autobracket` (no secret; keep a copy in your notes or a repo).

## What a run does

`autobracket-backup.sh` (bash, `set -euo pipefail`, every failure exits non-zero so the unit
shows up in `systemctl --failed`):

1. Reads `/etc/autobracket/backup.env` (see `deploy/backup/backup.env.example`; every
   setting has a default). Creates `BACKUP_DIR` with mode 0700; `umask 077` makes every
   file 0600.
2. `sqlite3 SQLITE_DB ".backup TMP"`: the online backup API copies a consistent snapshot
   while the server keeps running (a plain `cp` of a WAL-mode database can be torn). The copy
   is switched to `journal_mode=DELETE`, so it is one self-contained file, and checked with
   `PRAGMA integrity_check`; only when that prints `ok` is it renamed to
   `autobracket-YYYYMMDD-HHMMSS.sqlite`.
3. If `PG_CONTAINER` is set: `pg_dump --format=custom` of `PG_DATABASE` as `PG_USER`, run
   inside the container with `docker exec -i` (or `docker compose -f PG_COMPOSE_FILE exec -T`
   when `PG_COMPOSE_FILE` is set), written to a temporary file, checked to be a non-empty
   custom-format dump, renamed to `zitadel-YYYYMMDD-HHMMSS.pgdump`. With `PG_CONTAINER`
   empty this step logs "skipped" and the run still succeeds.
4. Keeps the newest `KEEP` (default 7) files of each kind and deletes the rest. Temporary
   files (`.*.tmp`) are removed on any exit.

The timer fires at 03:30 local time with up to 30 minutes of random delay
(`RandomizedDelaySec`) and catches up a missed night at the next boot (`Persistent=true`).
The service unit runs as root (it must read the `DynamicUser`'s private state directory and
reach Docker), with the rest of the file system read-only except `BACKUP_DIR` and the state
directory; a WAL reader writes to the database's `-shm` file, so the latter cannot be
read-only.

## Install

From a checkout, as root (the production machine's paths; see README "Deployment"):

```sh
sudo apt install sqlite3                                   # the CLI, if not present
sudo install -m 755 deploy/backup/autobracket-backup.sh /usr/local/bin/
sudo install -m 600 deploy/backup/backup.env.example /etc/autobracket/backup.env
sudo $EDITOR /etc/autobracket/backup.env                   # PG_CONTAINER etc., or leave the defaults
sudo install -m 644 deploy/backup/autobracket-backup.service deploy/backup/autobracket-backup.timer /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl start autobracket-backup.service            # one run now, to check it works
sudo ls -la /var/backups/autobracket
sudo systemctl enable --now autobracket-backup.timer
systemctl list-timers autobracket-backup.timer
```

If you change `BACKUP_DIR`, change `ReadWritePaths=` in the service unit to match (a
drop-in `systemctl edit autobracket-backup.service` works). Logs: `journalctl -u
autobracket-backup`.

Test the script without touching the system: `scripts/test-backup.sh` runs it against a
temporary database into a temporary directory and checks permissions, integrity, rotation,
the skipped-Postgres path and the Postgres path through a fake `docker`.

## Restore

### autobracket (SQLite)

The snapshot is a complete database file. Stop the server, keep the broken database, put the
snapshot in its place with the right owner, start the server:

```sh
sudo systemctl stop autobracket
cd /var/lib/private/autobracket            # the real StateDirectory (DynamicUser)
sudo mv autobracket.sqlite autobracket.sqlite.broken
sudo rm -f autobracket.sqlite-wal autobracket.sqlite-shm     # stale WAL would be applied to the restored file
sudo install -m 600 -o "$(stat -c %U .)" -g "$(stat -c %G .)" \
  /var/backups/autobracket/autobracket-YYYYMMDD-HHMMSS.sqlite autobracket.sqlite
sudo systemctl start autobracket
journalctl -u autobracket -n 20
```

The owner is the state directory's owner (the dynamic user; `stat -c %U .` prints it, which
may be a bare uid when the service is stopped: that is fine). The server re-enables WAL
mode on start. Results, events, players and replay records since the snapshot are gone;
replay files in `replays/` stay on disk but the database no longer knows the later ones.

To inspect a snapshot before restoring it, open a copy read-only:
`sqlite3 -readonly /var/backups/autobracket/autobracket-....sqlite 'select count(*) from events;'`.

### Zitadel (Postgres)

The dump is `pg_dump --format=custom`; restore it with `pg_restore` inside the same
container. Stop Zitadel first (not Postgres), so nothing writes during the restore:

```sh
docker stop zitadel            # the Zitadel application container, not the database
docker exec -i PG_CONTAINER pg_restore -U postgres --dbname=zitadel --clean --if-exists \
  --no-owner < /var/backups/autobracket/zitadel-YYYYMMDD-HHMMSS.pgdump
docker start zitadel
```

`--clean --if-exists` drops and recreates each object; to restore into a fresh database
instead, `createdb` one and point `--dbname` at it. Users, sessions and keys created after
the dump are gone; clients then have to sign in again. With `PG_COMPOSE_FILE`, replace
`docker exec -i PG_CONTAINER` with `docker compose -f FILE exec -T SERVICE`.
