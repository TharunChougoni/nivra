# Market archive database

The paper host runs PostgreSQL 18 with the TimescaleDB build from Timescale's
own apt repository. The Ubuntu and PGDG `postgresql-18-timescaledb` packages
are the Apache build without compression, so do not use them. TimescaleDB
builds for PostgreSQL 18.7 need PostgreSQL from PGDG.

```sh
sudo install -d -m 0755 /etc/apt/keyrings
curl -fsSL https://www.postgresql.org/media/keys/ACCC4CF8.asc \
  | sudo tee /etc/apt/keyrings/pgdg.asc >/dev/null
curl -fsSL https://packagecloud.io/timescale/timescaledb/gpgkey \
  | sudo gpg --dearmor --yes -o /etc/apt/keyrings/timescaledb.gpg
. /etc/os-release
echo "deb [signed-by=/etc/apt/keyrings/pgdg.asc] https://apt.postgresql.org/pub/repos/apt $VERSION_CODENAME-pgdg main" \
  | sudo tee /etc/apt/sources.list.d/pgdg.list
echo "deb [signed-by=/etc/apt/keyrings/timescaledb.gpg] https://packagecloud.io/timescale/timescaledb/ubuntu/ $VERSION_CODENAME main" \
  | sudo tee /etc/apt/sources.list.d/timescaledb.list
sudo apt-get update
sudo apt-get install -y --no-install-recommends postgresql-18 timescaledb-2-postgresql-18
```

Install the two fragments from this directory and restart the cluster:

```sh
sudo install -m 0644 nivra-archive.conf /etc/postgresql/18/main/conf.d/
sudo install -d /etc/systemd/system/postgresql@18-main.service.d
sudo install -m 0644 memory.conf /etc/systemd/system/postgresql@18-main.service.d/
sudo systemctl daemon-reload
sudo systemctl restart postgresql@18-main
```

Create the role and database. Keep the password in a 0600 file and name it in
`market_archive.password_file`:

```sh
umask 077
openssl rand -hex 24 > ~/.config/nivra/archive-password
sudo -u postgres psql -v ON_ERROR_STOP=1 \
  -v pw="$(cat ~/.config/nivra/archive-password)" <<'SQL'
CREATE ROLE nivra LOGIN PASSWORD :'pw';
CREATE DATABASE nivra_archive OWNER nivra;
\c nivra_archive
CREATE EXTENSION IF NOT EXISTS timescaledb;
SQL
```

Then set `market_archive` (see [configuration](../CONFIGURATION.md#market-archive))
and run `nivra archive init` and `nivra archive check`.
