#!/usr/bin/env python3
"""The demo cluster's Icinga objects (demo/docker-compose.yml), the same on
every run: about 260 hosts and 3 000 services of a production estate (web,
API, databases, Kubernetes, queues, storage, edge sites, network) spread
over the zones master, ams and fra, their groups, users and notifications,
the heartbeats icygui watches, and a few dependencies and scheduled
downtimes.

Usage: generate.py OUT_DIR    (writes OUT_DIR/<zone>/*.conf; the config
master runs it into /etc/icinga2/zones.d at every start)

The checks need nothing outside Icinga: every check command is Icinga's
built-in dummy check under the plugin's name (`disk`, `http`,
`check_postgres`, ...), whose state and output follow a schedule of its own
(`vars.demo`, see `demo_state` in global-templates/demo-functions.conf):

- most services are steady and OK;
- about one in eleven has a short problem now and then (a few minutes
  every few hours, at its own time), so a few problems come and go all
  the time;
- about one in a hundred flaps for half an hour every couple of hours;
- the problems listed in `STUCK` stay (they are what the seed script,
  seed.py, acknowledges, comments and puts in downtime: `SEED`).

The outputs of the standard Linux checks come from the scale benchmark's
generator (contract/scale/generate.py).
"""
import importlib.util
import os
import random
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
_SPEC = importlib.util.spec_from_file_location(
    "scale_generate", os.path.join(HERE, "..", "..", "contract", "scale", "generate.py"))
scale = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(scale)

SCALE_OK = dict(scale.SERVICES)
UNKNOWN = scale.PROBLEMS[3]

# --- Outputs: OK, WARNING, CRITICAL per service name --------------------------

TEXTS = {
    "ping4": (SCALE_OK["ping4"],
              "PING WARNING - Packet loss = 12%, RTA = 182.40 ms|rta=182.4ms;100;200;0 pl=12%;20;60;0",
              "PING CRITICAL - Packet loss = 100%|rta=0ms;100;200;0 pl=100%;20;60;0"),
    "ssh": (SCALE_OK["ssh"],
            "SSH WARNING - OpenSSH_9.6p1 (protocol 2.0) slow answer|time=4.012s;3;8;0;10",
            "CRITICAL - Socket timeout after 10 seconds"),
    "load": (SCALE_OK["load"],
             "WARNING - load average 14.2, 12.8, 11.1|load1=14.2;8;12;0 load5=12.8;6;10;0 load15=11.1;4;8;0",
             "CRITICAL - load average 31.7, 24.0, 15.2|load1=31.7;8;12;0 load5=24.0;6;10;0 load15=15.2;4;8;0"),
    "disk /": (SCALE_OK["disk /"],
               "DISK WARNING - free space: / 6 GiB (9% inode=88%)|/=64GiB;56;63;0;70",
               "DISK CRITICAL - free space: / 2 GiB (3% inode=86%)|/=68GiB;56;63;0;70"),
    "memory": (SCALE_OK["memory"],
               "WARNING - 88% used (14.0 GiB of 15.9 GiB)|used=15032385536B;13664501760;15374159872;0;17083817984",
               "CRITICAL - 97% used (15.4 GiB of 15.9 GiB)|used=16535624704B;13664501760;15374159872;0;17083817984"),
    "ntp-offset": ("NTP OK: Offset 0.004 secs|offset=0.004s;60;120;",
                   "NTP WARNING: Offset 72.3 secs|offset=72.3s;60;120;",
                   "NTP CRITICAL: Offset 141.0 secs|offset=141s;60;120;"),
    "apt": ("APT OK: 0 packages available for upgrade (0 critical updates).|available_upgrades=0;;;0 critical_updates=0;;;0",
            "APT WARNING: 14 packages available for upgrade (0 critical updates).|available_upgrades=14;;;0 critical_updates=0;;;0",
            "APT CRITICAL: 9 packages available for upgrade (3 critical updates).|available_upgrades=9;;;0 critical_updates=3;;;0"),
    "http": (SCALE_OK["http"],
             "HTTP WARNING: HTTP/1.1 200 OK - 4812 bytes in 1.412 second response time|time=1.412s;1;2;0 size=4812B;;;0",
             "HTTP CRITICAL: HTTP/1.1 503 Service Unavailable - 312 bytes in 0.008 second response time|time=0.008s;1;2;0 size=312B;;;0"),
    "http-tls": ("HTTP OK: HTTP/2 200 OK - 9211 bytes in 0.063 second response time|time=0.063s;1;2;0 size=9211B;;;0",
                 "HTTP WARNING: HTTP/2 200 OK - 9211 bytes in 1.731 second response time|time=1.731s;1;2;0 size=9211B;;;0",
                 "HTTP CRITICAL - SSL handshake failed: certificate verify failed"),
    "http-latency": ("HTTP OK: p95 latency 84 ms|p95=0.084s;0.5;1;0",
                     "HTTP WARNING: p95 latency 640 ms|p95=0.64s;0.5;1;0",
                     "HTTP CRITICAL: p95 latency 1840 ms|p95=1.84s;0.5;1;0"),
    "api-health": ("OK - /healthz answered 200 in 12 ms",
                   "WARNING - /healthz degraded: cache miss rate 31%",
                   "CRITICAL - /healthz answered 500: database pool exhausted"),
    "nginx-workers": ("PROCS OK: 9 processes with command name 'nginx'|procs=9;;;0",
                      "PROCS WARNING: 2 processes with command name 'nginx'|procs=2;;;0",
                      "PROCS CRITICAL: 0 processes with command name 'nginx'|procs=0;;;0"),
    "php-fpm": ("PROCS OK: 33 processes with command name 'php-fpm8.3'|procs=33;;;0",
                "PROCS WARNING: 120 processes with command name 'php-fpm8.3'|procs=120;100;140;0",
                "PROCS CRITICAL: 0 processes with command name 'php-fpm8.3'|procs=0;;;0"),
    "jvm-heap": ("JMX OK - heap 41% (1.6 GiB of 4.0 GiB)|heap=41%;85;95;0;100",
                 "JMX WARNING - heap 88% (3.5 GiB of 4.0 GiB)|heap=88%;85;95;0;100",
                 "JMX CRITICAL - heap 97% (3.9 GiB of 4.0 GiB), GC overhead 41%|heap=97%;85;95;0;100"),
    "cert-expiry": (SCALE_OK["cert"],
                    "SSL WARNING - Certificate expires in 21 days (2026-10-31)|days=21;30;7;0",
                    "SSL CRITICAL - Certificate expires in 3 days (2026-10-13)|days=3;30;7;0"),
    "postgres-replication": ("OK - standby lag 0s, slot active|replication_lag=0s;60;300",
                             "WARNING - standby lag 94s (> 60s)|replication_lag=94s;60;300",
                             "CRITICAL - standby lag 412s (> 300s)|replication_lag=412s;60;300"),
    "pg-connections": ("OK - 61 of 200 connections|connections=61;180;195;0;200",
                       "WARNING - 182 of 200 per-db limit (orders)|connections=182;180;195;0;200",
                       "CRITICAL - 197 of 200 connections|connections=197;180;195;0;200"),
    "pg-locks": ("POSTGRES_LOCKS OK: total=34|total=34;200;400", "POSTGRES_LOCKS WARNING: total=241 (exclusive=12)|total=241;200;400",
                 "POSTGRES_LOCKS CRITICAL: total=455 (exclusive=88)|total=455;200;400"),
    "pg-bloat": ("POSTGRES_BLOAT OK: largest table bloat 12%", "WARNING - check_postgres degraded: orders_archive bloat 41%",
                 "POSTGRES_BLOAT CRITICAL: orders_archive bloat 68% (wasted 92 GiB)"),
    "pg-backup-age": ("POSTGRES_BACKUP OK: last base backup 6h ago|age=21600s;93600;180000;0",
                      "POSTGRES_BACKUP WARNING: last base backup 27h ago|age=97200s;93600;180000;0",
                      "POSTGRES_BACKUP CRITICAL: last base backup 51h ago|age=183600s;93600;180000;0"),
    "pg-wal-archive": ("OK - WAL archive current, 0 files waiting|waiting=0;20;100;0", "WARNING - 37 WAL files waiting to be archived|waiting=37;20;100;0",
                       "CRITICAL - archive_command failing for 18 min, 212 files waiting|waiting=212;20;100;0"),
    "pgbouncer": ("PROCS OK: 1 process with command name 'pgbouncer'|procs=1;;;0", "PROCS WARNING: 2 processes with command name 'pgbouncer'|procs=2;;;0",
                  "PROCS CRITICAL: 0 processes with command name 'pgbouncer'|procs=0;;;0"),
    "pg-cache-hit": ("POSTGRES_HITRATIO OK: 99.4%|hitratio=99.4%;95;90", "POSTGRES_HITRATIO WARNING: 93.1%|hitratio=93.1%;95;90",
                     "POSTGRES_HITRATIO CRITICAL: 84.0%|hitratio=84%;95;90"),
    "pg-xid-age": ("POSTGRES_TXN_WRAPAROUND OK: 9% towards wraparound", "POSTGRES_TXN_WRAPAROUND WARNING: 52% towards wraparound",
                   "POSTGRES_TXN_WRAPAROUND CRITICAL: 81% towards wraparound"),
    "pg-autovacuum": ("POSTGRES_LAST_AUTOVACUUM OK: orders 41m ago", "WARNING - autovacuum on orders running for 2h 14m",
                      "CRITICAL - autovacuum on orders blocked for 5h"),
    "pg-checkpoints": ("POSTGRES_CHECKPOINT OK: last checkpoint 112s ago|age=112s;600;1200", "POSTGRES_CHECKPOINT WARNING: last checkpoint 744s ago|age=744s;600;1200",
                       "POSTGRES_CHECKPOINT CRITICAL: last checkpoint 1580s ago|age=1580s;600;1200"),
    "pg-deadlocks": ("OK - 0 deadlocks in 5 min|deadlocks=0;3;10;0", "WARNING - 4 deadlocks in 5 min|deadlocks=4;3;10;0",
                     "CRITICAL - 17 deadlocks in 5 min|deadlocks=17;3;10;0"),
    "pg-slow-queries": ("OK - 2 queries over 1 s in 5 min|slow=2;20;50;0", "WARNING - 31 queries over 1 s in 5 min|slow=31;20;50;0",
                        "CRITICAL - 88 queries over 1 s in 5 min|slow=88;20;50;0"),
    "pg-sequences": ("POSTGRES_SEQUENCE OK: most used 34%", "POSTGRES_SEQUENCE WARNING: orders_id_seq 87% used",
                     "POSTGRES_SEQUENCE CRITICAL: orders_id_seq 96% used"),
    "pg-replication-slots": ("OK - 2 slots active, 0 inactive|retained=0.2GiB;4;8", "WARNING - slot repl_db05 inactive, 4.4 GiB retained|retained=4.4GiB;4;8",
                             "CRITICAL - slot repl_db05 inactive, 9.1 GiB retained|retained=9.1GiB;4;8"),
    "mongodb-replset": ("OK - replica set rs0: 1 primary, 2 secondaries", "WARNING - replica set rs0: secondary db-mongo-03 lagging 41s",
                        "CRITICAL - replica set rs0: no primary"),
    "mongodb-connections": ("OK - 211 of 51200 connections (0%)|connections=211;40960;46080;0;51200", "WARNING - 41890 of 51200 connections (81%)|connections=41890;40960;46080;0;51200",
                            "CRITICAL - 47012 of 51200 connections (91%)|connections=47012;40960;46080;0;51200"),
    "mysql-replication": ("OK - replica in sync, lag 0s|lag=0s;60;300", "WARNING - replica lag 132s|lag=132s;60;300",
                          "CRITICAL - replication stopped: Error 1062 duplicate entry"),
    "mysql-connections": ("OK - 88 of 500 connections|connections=88;400;475;0;500", "WARNING - 431 of 500 connections|connections=431;400;475;0;500",
                          "CRITICAL - 491 of 500 connections|connections=491;400;475;0;500"),
    "kubelet": ("OK - kubelet healthy, 37 pods running|pods=37;100;110;0;110", "WARNING - kubelet PLEG not healthy for 4m",
                "CRITICAL - kubelet not ready: container runtime down"),
    "containerd": ("PROCS OK: 1 process with command name 'containerd'|procs=1;;;0", "PROCS WARNING: 2 processes with command name 'containerd'|procs=2;;;0",
                   "PROCS CRITICAL: 0 processes with command name 'containerd'|procs=0;;;0"),
    "disk /var": (SCALE_OK["disk /var"], "DISK WARNING - free space: /var 21 GiB (11% inode=92%)|/var=169GiB;152;171;0;190",
                  "DISK CRITICAL - free space: /var 3 GiB (2% inode=71%)|/var=187GiB;152;171;0;190"),
    "k8s-pods": ("OK - 0 pods pending, 0 crash-looping|pending=0;5;20;0 crashloop=0;1;3;0", "WARNING - 7 pods pending (insufficient memory)|pending=7;5;20;0 crashloop=0;1;3;0",
                 "CRITICAL - 4 pods in CrashLoopBackOff (payments-api)|pending=1;5;20;0 crashloop=4;1;3;0"),
    "kube-apiserver": ("HTTP OK: /readyz ok in 0.008 s|time=0.008s;0.5;1;0", "HTTP WARNING: /readyz slow, 0.742 s|time=0.742s;0.5;1;0",
                       "HTTP CRITICAL: /readyz failed: etcd not reachable"),
    "etcd": ("OK - etcd cluster healthy, 3 members, leader k8s-cp-01", "WARNING - etcd fsync p99 112 ms|fsync_p99=0.112s;0.1;0.5;0",
             "CRITICAL - etcd member k8s-cp-02 unhealthy: no leader"),
    "kube-scheduler": ("OK - kube-scheduler leader elected, queue 0", "WARNING - 41 pods waiting for scheduling",
                       "CRITICAL - kube-scheduler has no leader"),
    "kube-controller-manager": ("OK - controller-manager healthy", "WARNING - controller-manager work queue depth 220",
                                "CRITICAL - controller-manager unhealthy: leader lease lost"),
    "rabbitmq-queue": ("OK - queue orders.retry depth 12|depth=12;5000;10000;0", "WARNING - queue orders.retry depth 6,210|depth=6210;5000;10000;0",
                       "CRITICAL - queue orders.retry depth 18,402|depth=18402;5000;10000;0"),
    "rabbitmq-cluster": ("OK - 5 nodes running, no partitions", "WARNING - node mq-prod-04 alarm: memory high watermark",
                         "CRITICAL - network partition detected (mq-prod-02, mq-prod-05)"),
    "rabbitmq-memory": ("OK - memory 38% of high watermark|mem=38%;80;95;0;100", "WARNING - memory 84% of high watermark|mem=84%;80;95;0;100",
                        "CRITICAL - memory 97% of high watermark, publishers blocked|mem=97%;80;95;0;100"),
    "kafka-broker": ("OK - broker 1 online, 412 partitions, 0 offline", "WARNING - broker 2 under-replicated partitions: 14",
                     "CRITICAL - broker 3 offline"),
    "kafka-isr": ("OK - all partitions in sync (ISR 3/3)", "WARNING - 9 partitions with ISR 2/3", "CRITICAL - 3 partitions with ISR 1/3 (min.insync 2)"),
    "kafka-consumer-lag": ("OK - consumer group billing lag 31|lag=31;10000;50000;0", "WARNING - consumer group billing lag 28,114|lag=28114;10000;50000;0",
                           "CRITICAL - consumer group billing lag 91,880|lag=91880;10000;50000;0"),
    "redis-memory": ("OK - used_memory 1.2 GiB of 8 GiB (15%)|used=15%;80;95;0;100", "WARNING - used_memory 6.9 GiB of 8 GiB (86%)|used=86%;80;95;0;100",
                     "CRITICAL - used_memory 7.8 GiB of 8 GiB (97%), evicting keys|used=97%;80;95;0;100"),
    "redis-replication": ("OK - role master, 2 replicas online", "WARNING - replica cache-03 lag 14s", "CRITICAL - replica cache-04 link down for 310s"),
    "redis-connections": ("OK - 141 clients|clients=141;5000;9000;0", "WARNING - 5,412 clients|clients=5412;5000;9000;0",
                          "CRITICAL - 9,880 clients, rejecting connections|clients=9880;5000;9000;0"),
    "haproxy-backend": ("HAPROXY OK - all backends up (12/12)", "HAPROXY WARNING - backend api: 2 of 8 servers down",
                        "HAPROXY CRITICAL - backend web: no server available"),
    "haproxy-frontend": ("HAPROXY OK - 1,812 sessions (18%)|sessions=1812;8000;9500;0;10000", "HAPROXY WARNING - 8,410 sessions (84%)|sessions=8410;8000;9500;0;10000",
                         "HAPROXY CRITICAL - 9,911 sessions (99%)|sessions=9911;8000;9500;0;10000"),
    "keepalived": ("PROCS OK: 2 processes with command name 'keepalived'|procs=2;;;0", "PROCS WARNING: VRRP instance VI_1 in FAULT state",
                   "PROCS CRITICAL: 0 processes with command name 'keepalived'|procs=0;;;0"),
    "wireguard-peers": ("OK - 37 peers, last handshake < 3 min|peers=37;;;0", "WARNING - 4 peers without handshake for 10 min",
                        "CRITICAL - interface wg0 down"),
    "openvpn": ("PROCS OK: 1 process with command name 'openvpn'|procs=1;;;0", "PROCS WARNING: 3 processes with command name 'openvpn'|procs=3;;;0",
                "PROCS CRITICAL: 0 processes with command name 'openvpn'|procs=0;;;0"),
    "zfs-pool": ("OK - pool tank ONLINE, 41% used|used=41%;80;90;0;100", "WARNING - pool tank DEGRADED: 1 of 12 disks faulted",
                 "CRITICAL - pool tank FAULTED: 3 of 12 disks unavailable"),
    "smart-disks": (SCALE_OK["smart"], "WARNING - /dev/sdf: 8 reallocated sectors", "CRITICAL - /dev/sdc: SMART overall-health FAILED"),
    "disk /srv": ("DISK OK - free space: /srv 4.1 TiB (51% inode=99%)|/srv=4.0TiB;7.2;7.6;0;8.1",
                  "DISK WARNING - free space: /srv 0.6 TiB (7% inode=99%)|/srv=7.5TiB;7.2;7.6;0;8.1",
                  "DISK CRITICAL - free space: /srv 0.2 TiB (2% inode=99%)|/srv=7.9TiB;7.2;7.6;0;8.1"),
    "nfs-exports": ("OK - 6 exports, 41 clients", "WARNING - export /srv/home: 3 stale client handles", "CRITICAL - nfsd not responding"),
    "minio-health": ("HTTP OK: /minio/health/cluster 200 in 0.012 s|time=0.012s;1;2;0", "HTTP WARNING: 1 of 4 drives offline, quorum kept",
                     "HTTP CRITICAL: /minio/health/cluster 503: write quorum lost"),
    "disk /data": ("DISK OK - free space: /data 2.2 TiB (55% inode=99%)|/data=1.8TiB;3.2;3.6;0;4.0",
                   "DISK WARNING - free space: /data 0.4 TiB (10% inode=99%)|/data=3.6TiB;3.2;3.6;0;4.0",
                   "DISK CRITICAL - free space: /data 0.1 TiB (2% inode=99%)|/data=3.9TiB;3.2;3.6;0;4.0"),
    "borg-last-run": (SCALE_OK["backup"], "WARNING - last backup 28h ago|age=100800s;86400;172800;0", "CRITICAL - last backup failed: repository locked"),
    "backup-disk": ("DISK OK - free space: /backup 9.4 TiB (39% inode=99%)|/backup=14.6TiB;20;22;0;24",
                    "DISK WARNING - free space: /backup 2.1 TiB (9% inode=99%)|/backup=21.9TiB;20;22;0;24",
                    "DISK CRITICAL - free space: /backup 0.7 TiB (3% inode=99%)|/backup=23.3TiB;20;22;0;24"),
    "restic-check": ("OK - restic check: no errors, 1,204 snapshots", "WARNING - restic check: 2 snapshots with missing packs",
                     "CRITICAL - repository locked by a stale job (pid 31244)"),
    "varnish": ("PROCS OK: 2 processes with command name 'varnishd'|procs=2;;;0", "WARNING - varnish hit rate 61%|hitrate=61%;70;50;0;100",
                "PROCS CRITICAL: 0 processes with command name 'varnishd'|procs=0;;;0"),
    "snmp-uptime": ("SNMP OK - Timeticks: (2290127800) 265 days, 1:27:58.00", "SNMP WARNING - device rebooted 4 minutes ago",
                    "SNMP CRITICAL - No response from remote host"),
    "cpu": ("SNMP OK - CPU 7% (5 min)|cpu=7%;80;90;0;100", "SNMP WARNING - CPU 84% (5 min)|cpu=84%;80;90;0;100", "SNMP CRITICAL - CPU 96% (5 min)|cpu=96%;80;90;0;100"),
    "temperature": ("SNMP OK - inlet 24 C|temp=24;40;50", "SNMP WARNING - inlet 43 C|temp=43;40;50", "SNMP CRITICAL - inlet 54 C|temp=54;40;50"),
    "psu": ("SNMP OK - PSU 1 ok, PSU 2 ok", "SNMP WARNING - PSU 2 no input power", "SNMP CRITICAL - both power supplies failed"),
    "uplink": ("SNMP OK - Te1/1/1 up, 2.1 Gbit/s in, 1.4 Gbit/s out, 0 errors|in=2.1Gb out=1.4Gb errors=0",
               "SNMP WARNING - Te1/1/1 up, 214 CRC errors in 5 min|errors=214;100;1000",
               "SNMP CRITICAL - Te1/1/1 down"),
    "archive-verify": ("OK - archive verified", "WARNING - archive verification slow", "CRITICAL - archive verification failed"),
}

# --- The estate ----------------------------------------------------------------

STANDARD = [("ping4", "ping4", 30), ("ssh", "ssh", 120), ("load", "load", 60),
            ("disk /", "disk", 300), ("memory", "mem", 120), ("ntp-offset", "ntp_time", 300),
            ("apt", "apt", 1800)]

# (service, check command, service groups, check interval s)
ROLE_SERVICES = {
    "web-edge": [("http", "http", ["http"], 30), ("http-tls", "http", ["http", "tls-certificates"], 60),
                 ("nginx-workers", "procs", ["http"], 60), ("cert-expiry", "ssl_cert", ["tls-certificates"], 3600)],
    "web": [("http", "http", ["http"], 30), ("nginx-workers", "procs", ["http"], 60), ("php-fpm", "procs", ["http"], 60)],
    "api": [("http", "http", ["http"], 30), ("http-latency", "http", ["http"], 60),
            ("api-health", "http", ["http"], 60), ("jvm-heap", "jmx", [], 60)],
    "api-gateway": [("http", "http", ["http"], 30), ("http-latency", "http", ["http"], 60),
                    ("http-tls", "http", ["http", "tls-certificates"], 60)],
    "postgres": [("postgres-replication", "check_postgres", ["databases", "replication"], 60),
                 ("pg-connections", "check_postgres", ["databases"], 60),
                 ("pg-locks", "check_postgres", ["databases"], 60),
                 ("pg-bloat", "check_postgres", ["databases"], 900),
                 ("pg-backup-age", "check_postgres", ["databases", "backups"], 900),
                 ("pg-wal-archive", "check_postgres", ["databases", "replication"], 120),
                 ("pgbouncer", "procs", ["databases"], 60),
                 ("pg-cache-hit", "check_postgres", ["databases"], 300),
                 ("pg-xid-age", "check_postgres", ["databases"], 900),
                 ("pg-autovacuum", "check_postgres", ["databases"], 300),
                 ("pg-checkpoints", "check_postgres", ["databases"], 300),
                 ("pg-deadlocks", "check_postgres", ["databases"], 300),
                 ("pg-slow-queries", "check_postgres", ["databases"], 300),
                 ("pg-sequences", "check_postgres", ["databases"], 900),
                 ("pg-replication-slots", "check_postgres", ["databases", "replication"], 120)],
    "mongodb": [("mongodb-replset", "mongodb", ["databases", "replication"], 60),
                ("mongodb-connections", "mongodb", ["databases"], 60)],
    "mysql": [("mysql-replication", "mysql", ["databases", "replication"], 60),
              ("mysql-connections", "mysql", ["databases"], 60)],
    "k8s-node": [("kubelet", "kubelet", ["kubernetes"], 60), ("containerd", "procs", ["kubernetes"], 60),
                 ("disk /var", "disk", ["kubernetes"], 300), ("k8s-pods", "kubernetes", ["kubernetes"], 60)],
    "k8s-control-plane": [("kube-apiserver", "http", ["kubernetes"], 30), ("etcd", "etcd", ["kubernetes"], 60),
                          ("kube-scheduler", "kubernetes", ["kubernetes"], 60),
                          ("kube-controller-manager", "kubernetes", ["kubernetes"], 60)],
    "rabbitmq": [("rabbitmq-queue", "rabbitmq", ["queue"], 60), ("rabbitmq-cluster", "rabbitmq", ["queue"], 60),
                 ("rabbitmq-memory", "rabbitmq", ["queue"], 60)],
    "kafka": [("kafka-broker", "kafka", ["queue"], 60), ("kafka-isr", "kafka", ["queue"], 60),
              ("kafka-consumer-lag", "kafka", ["queue"], 60)],
    "redis": [("redis-memory", "redis", ["cache"], 60), ("redis-replication", "redis", ["cache", "replication"], 60),
              ("redis-connections", "redis", ["cache"], 60)],
    "haproxy": [("haproxy-backend", "haproxy", ["loadbalancing"], 30), ("haproxy-frontend", "haproxy", ["loadbalancing"], 60),
                ("keepalived", "procs", ["loadbalancing"], 60)],
    "vpn": [("cert-expiry", "ssl_cert", ["tls-certificates"], 3600), ("wireguard-peers", "wireguard", [], 120),
            ("openvpn", "procs", [], 60)],
    "storage": [("zfs-pool", "zfs", ["storage"], 300), ("smart-disks", "smart", ["storage"], 900),
                ("disk /srv", "disk", ["storage"], 300)],
    "nfs": [("nfs-exports", "nfs", ["storage"], 120), ("disk /srv", "disk", ["storage"], 300)],
    "minio": [("minio-health", "http", ["storage"], 60), ("disk /data", "disk", ["storage"], 300)],
    "backup": [("borg-last-run", "borg", ["backups"], 900), ("backup-disk", "disk", ["backups"], 300),
               ("restic-check", "restic", ["backups"], 900)],
    "edge": [("http", "http", ["http"], 30), ("http-tls", "http", ["http", "tls-certificates"], 60),
             ("cert-expiry", "ssl_cert", ["tls-certificates"], 3600), ("varnish", "procs", ["http"], 60)],
    "switch": [("snmp-uptime", "snmp", ["network"], 120), ("cpu", "snmp", ["network"], 120),
               ("temperature", "snmp", ["network"], 300), ("psu", "snmp", ["network"], 300)],
}

TEAMS = {"web-edge": "web", "web": "web", "api": "web", "api-gateway": "web", "postgres": "dba",
         "mongodb": "dba", "mysql": "dba", "redis": "dba", "k8s-node": "platform",
         "k8s-control-plane": "platform", "rabbitmq": "platform", "kafka": "platform",
         "haproxy": "network", "vpn": "network", "switch": "network", "edge": "network",
         "storage": "storage", "nfs": "storage", "minio": "storage", "backup": "storage",
         "monitoring": "platform"}

HOST_GROUPS = {"web-edge": ["web-frontend"], "web": ["web-frontend"], "api": ["api"],
               "api-gateway": ["api"], "postgres": ["databases"], "mongodb": ["databases"],
               "mysql": ["databases"], "redis": ["cache"], "k8s-node": ["kubernetes"],
               "k8s-control-plane": ["kubernetes"], "rabbitmq": ["queue"], "kafka": ["queue"],
               "haproxy": ["loadbalancers"], "vpn": ["vpn"], "storage": ["storage"],
               "nfs": ["storage"], "minio": ["storage"], "backup": ["storage"],
               "switch": ["network"], "monitoring": ["monitoring"]}

# Hosts the contract fixtures already define (contract/icinga/icygui-test.conf).
FIXTURE_HOSTS = {"db-prod-03", "k8s-node-07", "k8s-node-11", "behind-node-11"}


class HostDef:
    def __init__(self, name, address, role, zone, site, env="prod", groups=None, extra=None):
        self.name, self.address, self.role, self.zone = name, address, role, zone
        self.site, self.env = site, env
        self.groups = list(groups if groups is not None else HOST_GROUPS.get(role, []))
        self.extra = dict(extra or {})
        self.linux = role != "switch"


def estate():
    hosts = []

    def many(prefix, count, net, start, role, zone="master", site="core", **kw):
        for i in range(1, count + 1):
            name = f"{prefix}-{i:02d}"
            if name not in FIXTURE_HOSTS:
                hosts.append(HostDef(name, f"{net}.{start + i}", role, zone, site, **kw))

    many("web-edge", 4, "10.0.1", 10, "web-edge")
    many("web-prod", 48, "10.0.1", 20, "web")
    many("api-gw", 2, "10.0.3", 10, "api-gateway")
    many("api-prod", 24, "10.0.3", 20, "api")
    for i in range(1, 7):
        name = f"db-prod-{i:02d}"
        if name in FIXTURE_HOSTS:
            continue
        cluster = "orders-main" if i <= 3 else "billing"
        hosts.append(HostDef(name, f"10.0.2.{10 + i}", "postgres", "master", "core",
                             extra={"pg_cluster": cluster, "lag_crit": 300,
                                    "runbook": "https://wiki.example.com/db/replication-lag"}))
    many("db-mongo", 3, "10.0.2", 30, "mongodb")
    many("db-mysql", 3, "10.0.2", 40, "mysql")
    many("k8s-cp", 3, "10.0.4", 10, "k8s-control-plane")
    many("k8s-node", 72, "10.0.4", 20, "k8s-node")
    many("mq-prod", 5, "10.0.5", 10, "rabbitmq")
    many("kafka", 3, "10.0.5", 30, "kafka")
    many("cache", 4, "10.0.7", 10, "redis")
    many("lb-prod", 2, "10.0.0", 10, "haproxy")
    many("vpn-gw", 2, "10.0.0", 20, "vpn")
    many("nfs", 2, "10.0.6", 10, "nfs")
    many("minio", 4, "10.0.6", 20, "minio")
    many("backup", 2, "10.0.6", 40, "backup")
    # The lab: nobody is notified, a frozen environment.
    for i in range(1, 13):
        role = ["web", "api", "postgres", "k8s-node"][i % 4]
        groups = HOST_GROUPS.get(role, []) + ["lab"]
        hosts.append(HostDef(f"lab-{role.split('-')[0]}-{i:02d}", f"10.20.0.{10 + i}", role,
                             "master", "core", env="lab", groups=groups))
    # Amsterdam, behind the satellite sat-ams-01.
    many("edge-ams", 14, "10.8.2", 10, "edge", zone="ams", site="ams", groups=["edge-ams"])
    many("store-ams", 4, "10.8.1", 10, "storage", zone="ams", site="ams")
    many("db-ams", 2, "10.8.3", 10, "postgres", zone="ams", site="ams",
         extra={"pg_cluster": "orders-main", "lag_crit": 300})
    many("sw-core-ams", 2, "10.8.0", 1, "switch", zone="ams", site="ams")
    # Frankfurt, behind the HA satellites sat-fra-01 and sat-fra-02.
    many("edge-fra", 16, "185.22.10", 0, "edge", zone="fra", site="fra", groups=["edge-fra"])
    many("store-fra", 4, "10.9.1", 10, "storage", zone="fra", site="fra")
    many("k8s-fra-node", 24, "10.9.4", 20, "k8s-node", zone="fra", site="fra")
    many("sw-core-fra", 2, "10.9.0", 1, "switch", zone="fra", site="fra")
    return hosts


# The Icinga nodes themselves, monitored from the master zone: `icinga`
# runs on each node (command_endpoint), `cluster-zone` checks that a
# satellite zone is connected.
NODES = [("master-01", "10.0.0.5", "master"), ("master-02", "10.0.0.6", "master"),
         ("sat-ams-01", "10.8.0.5", "ams"), ("sat-fra-01", "10.9.0.5", "fra"),
         ("sat-fra-02", "10.9.0.6", "fra")]

# --- Problems that stay, and what the seed does about them --------------------

# (host, service or None for the host, state, output or None for the table's)
STUCK = [
    ("mq-prod-01", "rabbitmq-queue", 2, None),
    ("db-prod-01", "pg-autovacuum", 1, None),
    ("db-prod-05", "pg-bloat", 1, None),
    ("db-prod-02", "load", 1, None),
    ("backup-02", "restic-check", 2, None),
    ("edge-fra-05", "cert-expiry", 1, None),
    ("kafka-02", "kafka-consumer-lag", 1, None),
    ("k8s-node-23", "k8s-pods", 2, None),
    ("api-prod-09", "jvm-heap", 1, None),
    ("store-ams-03", None, 2, "PING CRITICAL - Packet loss = 100%"),
    ("store-ams-03", "zfs-pool", 2, None),
    ("edge-ams-07", "http", 2, None),
    ("web-prod-17", "php-fpm", 2, None),
    ("cache-03", "redis-replication", 1, None),
    ("db-mongo-02", "mongodb-connections", 1, None),
    ("k8s-fra-node-14", None, 2, "PING CRITICAL - Packet loss = 100%"),
    ("lab-postgres-02", "pg-replication-slots", 2, None),
    ("lab-web-04", None, 2, "PING CRITICAL - Packet loss = 100%"),
    ("minio-03", "minio-health", 1, None),
    ("vpn-gw-02", "cert-expiry", 1, None),
    ("sw-core-fra-02", "psu", 1, None),
    ("web-edge-03", "http-tls", 1, None),
]

# What seed.py does once the cluster has checked everything. Times are
# minutes from now; authors are the people of USERS.
SEED = {
    "results": [  # passive results for the contract fixtures' passive services
        ("db-prod-03", "postgres-replication", 2,
         "CRITICAL - standby lag 412s (> 300s)\nprimary  db-prod-01  lsn 4A/9C21F0D8\n"
         "standby  db-prod-03  lsn 4A/2E77A120\nslot     repl_db03   retained 1.8 GiB",
         ["replication_lag=412s;60;300", "wal_retained=1.8GiB;4;8"]),
        ("db-prod-03", "pg-connections", 1, "WARNING - 182 of 200 per-db limit (orders)",
         ["connections=182;180;195;0;200"]),
        ("k8s-node-07", "disk /var", 2,
         "DISK CRITICAL - free space: /var 3 GiB (2% inode=71%)", ["/var=187GiB;152;171;0;190"]),
    ],
    "acknowledgements": [
        ("db-prod-01", "pg-autovacuum", "dba-oncall", "vacuum running on orders, about 30 minutes"),
        ("backup-02", "restic-check", "m.keller", "repository locked by a stale job, cleaning up"),
        ("edge-fra-05", "cert-expiry", "a.ivanova", "renewal ticket SEC-1182, new certificate on Monday"),
        ("kafka-02", "kafka-consumer-lag", "j.berg", "billing consumers scaled up, lag going down"),
        ("k8s-node-11", None, "infra-oncall", "node drained for a kernel update; replacement ordered"),
        ("cache-03", "redis-replication", "dba-oncall", "replica resync after the failover, ETA 20 min"),
        ("api-prod-09", "jvm-heap", "infra-oncall", "heap dump taken, restart after the release freeze"),
        ("sw-core-fra-02", "psu", "a.ivanova", "PSU 2 replacement with the datacenter (ticket DC-8812)"),
    ],
    "comments": [
        ("db-prod-03", "postgres-replication", "j.berg", "Failover drill on db-prod-01 at 15:00. Expect lag on 03."),
        ("mq-prod-01", "rabbitmq-queue", "infra-oncall", "consumer deploy rolled back, watching the backlog"),
        ("mq-prod-01", "rabbitmq-queue", "j.berg", "orders team says retries are safe to drop after 18:00"),
        ("web-prod-12", None, "m.keller", "decommission planned for next week (CHG-2210)"),
        ("k8s-node-23", "k8s-pods", "infra-lead", "payments-api crash loop, dev team paged"),
        ("db-prod-05", "pg-bloat", "dba-oncall", "VACUUM FULL scheduled in the downtime below"),
    ],
    "downtimes": [
        # (host, service or None, author, comment, start, end, flexible minutes, all services)
        ("db-prod-05", "pg-bloat", "dba-oncall", "VACUUM FULL on orders_archive, flexible 2h", -30, 210, 120, False),
        ("store-ams-03", None, "infra-oncall", "disk replacement in progress (RMA 4471)", -40, 80, 0, True),
        ("edge-ams-07", None, "a.ivanova", "provider maintenance window AMS-IX port 3", -15, 225, 0, True),
        ("k8s-node-31", None, "infra-lead", "kernel update, rolling", 150, 240, 0, True),
        ("k8s-node-32", None, "infra-lead", "kernel update, rolling", 165, 255, 0, True),
        ("k8s-node-33", None, "infra-lead", "kernel update, rolling", 180, 270, 0, True),
        ("lab-web-04", None, "m.keller", "lab frozen until the migration", -1440, 8640, 0, True),
        ("lab-postgres-02", None, "m.keller", "lab frozen until the migration", -1440, 8640, 0, True),
        ("web-prod-17", "php-fpm", "j.berg", "php 8.3 rollout, restarts expected", -5, 55, 0, False),
        ("minio-03", None, "infra-oncall", "drive swap, quorum kept", 60, 120, 0, True),
    ],
}

USERS = [("dba-oncall", "DBA on-call"), ("infra-oncall", "Infrastructure on-call"),
         ("infra-lead", "Infrastructure lead"), ("m.keller", "Mara Keller"),
         ("j.berg", "Jonas Berg"), ("a.ivanova", "Anna Ivanova")]
USER_GROUPS = {"dba": ["dba-oncall", "j.berg"], "platform": ["infra-oncall", "infra-lead"],
               "storage": ["infra-oncall", "m.keller"], "web": ["infra-oncall", "j.berg"],
               "network": ["a.ivanova", "infra-oncall"]}

GROUP_NAMES = {  # host groups (databases and kubernetes are the fixtures')
    "web-frontend": "Web frontend", "api": "API", "queue": "Queues", "cache": "Caches",
    "loadbalancers": "Load balancers", "vpn": "VPN", "storage": "Storage",
    "edge-ams": "Edge Amsterdam", "edge-fra": "Edge Frankfurt", "network": "Network",
    "monitoring": "Monitoring", "linux-servers": "Linux servers", "lab": "Lab"}
SERVICE_GROUP_NAMES = {  # service groups (storage and replication are the fixtures')
    "databases": "Databases", "kubernetes": "Kubernetes", "http": "HTTP",
    "tls-certificates": "TLS certificates", "backups": "Backups",
    "loadbalancing": "Load balancing", "queue": "Queues", "cache": "Caches",
    "network": "Network", "monitoring": "Monitoring"}


def q(text):
    """`text` as an Icinga string literal."""
    return '"' + text.replace("\\", "\\\\").replace('"', '\\"').replace("\n", "\\n").replace("$", "$$") + '"'


def dsl(value):
    """A Python value as Icinga DSL."""
    if isinstance(value, bool):
        return "true" if value else "false"
    if isinstance(value, (int, float)):
        return str(value)
    if isinstance(value, str):
        return q(value)
    if isinstance(value, list):
        return "[ " + ", ".join(dsl(v) for v in value) + " ]"
    if isinstance(value, dict):
        return "{ " + ", ".join(f"{k} = {dsl(v)}" for k, v in value.items()) + " }"
    raise TypeError(value)


class Schedules:
    """Each check's schedule (`vars.demo`), drawn from one seeded generator
    in a fixed order, so every run writes the same."""

    def __init__(self):
        self.rng = random.Random(2026)
        self.stuck = {(h, s): (state, text) for h, s, state, text in STUCK}

    def service(self, host, name, interval):
        rng = self.rng
        r = rng.random()
        if (host, name) in self.stuck:
            state, text = self.stuck[(host, name)]
            demo = {"profile": "stuck", "state": state}
            if text:
                demo["text"] = text
            return demo
        if r < 0.010:
            return {"profile": "flap", "state": 2, "period": rng.choice([5400, 7200, 10800]),
                    "window": rng.randint(25, 40) * 60, "offset": rng.randrange(10800),
                    "step": max(interval, 30)}
        if r < 0.100:
            state = rng.choices([2, 1, 3], weights=[55, 40, 5])[0]
            period = rng.choice([7200, 10800, 14400, 21600, 28800])
            return {"profile": "blip", "state": state, "period": period,
                    "window": rng.randint(4, 25) * 60, "offset": rng.randrange(period)}
        return {"profile": "ok"}

    def host(self, host):
        rng = self.rng
        r = rng.random()
        if (host, None) in self.stuck:
            state, text = self.stuck[(host, None)]
            return {"profile": "stuck", "state": state, "text": text}
        if r < 0.02:
            return {"profile": "blip", "state": 2, "period": 21600,
                    "window": rng.randint(6, 15) * 60, "offset": rng.randrange(21600)}
        return {"profile": "ok"}


def host_block(h, demo):
    groups = h.groups + (["linux-servers"] if h.linux else [])
    lines = [f"object Host {q(h.name)} {{",
             '  import "demo-host"',
             f"  address = {q(h.address)}",
             f"  groups = {dsl(groups)}",
             f"  vars.role = {q(h.role)}",
             f"  vars.env = {q(h.env)}",
             f"  vars.os = {q('Linux' if h.linux else 'IOS-XE')}",
             f"  vars.site = {q(h.site)}",
             f"  vars.team = {q(TEAMS.get(h.role, 'platform'))}"]
    for key, value in h.extra.items():
        lines.append(f"  vars.{key} = {dsl(value)}")
    if h.role in ("postgres", "mysql", "mongodb"):
        lines.append(f"  notes_url = {q('https://wiki.example.com/db/' + h.name)}")
    lines.append(f"  vars.demo = {dsl(demo)}")
    lines.append("}")
    return "\n".join(lines)


def service_block(host, name, command, groups, interval, demo, extra=None):
    lines = [f"object Service {q(name)} {{",
             '  import "demo-service"',
             f"  host_name = {q(host)}",
             f"  check_command = {q(command)}",
             f"  check_interval = {interval}s",
             f"  retry_interval = {min(interval, 30)}s"]
    if groups:
        lines.append(f"  groups = {dsl(groups)}")
    if demo["profile"] == "flap":
        lines.append("  max_check_attempts = 1")
        lines.append("  enable_flapping = true")
    for key, value in (extra or {}).items():
        lines.append(f"  {key} = {value}")
    lines.append(f"  vars.demo = {dsl(demo)}")
    lines.append("}")
    return "\n".join(lines)


def services_of(h):
    services = []
    if h.linux:
        services += [(name, command, [], interval) for name, command, interval in STANDARD]
    else:
        services.append(("ping4", "ping4", ["network"], 30))
    services += ROLE_SERVICES.get(h.role, [])
    if h.role == "switch":
        uplink = f"uplink edge-{h.site}"
        services.append((uplink, "snmp-interface", ["network"], 60))
    return services


GLOBAL_FUNCTIONS = r'''/* How the demo's checks behave (demo/icinga/generate.py): every check
 * command is Icinga's built-in dummy check, its state and output follow
 * the schedule in the object's `vars.demo`:
 *   ok     always OK (UP);
 *   stuck  always `state` (output `text` or the table's);
 *   blip   `state` for `window` seconds once every `period` seconds,
 *          starting `offset` seconds into the period;
 *   flap   during that window, `state` and OK by turns every `step`
 *          seconds.
 * Nothing outside Icinga is involved, every node computes the same. */

globals.demo_state = function(demo) {
  if (typeof(demo) != Dictionary || demo.profile == "ok") { return 0 }
  if (demo.profile == "stuck") { return demo.state }
  var now = Math.floor(get_time())
  if ((now + demo.offset) % demo.period >= demo.window) { return 0 }
  if (demo.profile == "flap" && Math.floor(now / demo.step) % 2 == 1) { return 0 }
  return demo.state
}

globals.demo_output = function(demo, texts) {
  var state = demo_state(demo)
  if (state != 0 && typeof(demo) == Dictionary && demo.contains("text")) { return demo.text }
  return texts[state]
}
'''


def write(out, zone, name, text):
    directory = os.path.join(out, zone)
    os.makedirs(directory, exist_ok=True)
    with open(os.path.join(directory, name), "w") as file:
        file.write(text)


def main(out):
    hosts = estate()
    schedules = Schedules()
    by_zone = {"master": [], "ams": [], "fra": []}
    commands = {}  # check command -> {service name: texts}
    for h in hosts:
        blocks = [host_block(h, schedules.host(h.name))]
        for name, command, groups, interval in services_of(h):
            demo = schedules.service(h.name, name, interval)
            texts = TEXTS.get(name.split(" edge-")[0] if name.startswith("uplink") else name)
            if texts is None:
                raise SystemExit(f"generate.py: no outputs for {name}")
            commands.setdefault(command, {})[name] = texts
            blocks.append(service_block(h.name, name, command, groups, interval, demo))
        by_zone[h.zone].append("\n".join(blocks))
    # A passive check nobody has submitted yet: pending, as such checks are.
    by_zone["master"].append(service_block(
        "backup-01", "archive-verify", "passive-archive", ["backups"], 3600, {"profile": "ok"},
        {"enable_active_checks": "false"}))
    commands["passive-archive"] = {"archive-verify": TEXTS["archive-verify"]}

    header = "/* Generated by demo/icinga/generate.py; changes are overwritten at the next start. */\n\n"
    for zone, blocks in by_zone.items():
        write(out, zone, "hosts.conf", header + "\n\n".join(blocks) + "\n")

    # The nodes, from the master zone.
    nodes = []
    for name, address, zone in NODES:
        nodes.append(f'''object Host {q(name)} {{
  import "demo-host"
  address = {q(address)}
  groups = [ "monitoring", "linux-servers" ]
  vars.role = "monitoring"
  vars.env = "prod"
  vars.os = "Linux"
  vars.site = {q("core" if zone == "master" else zone)}
  vars.team = "platform"
  vars.demo = {{ profile = "ok" }}
}}
object Service "icinga" {{
  host_name = {q(name)}
  check_command = "icinga"
  command_endpoint = {q(name)}
  check_interval = 60s
  retry_interval = 30s
  groups = [ "monitoring" ]
}}''')
        if zone != "master":
            nodes.append(f'''object Service "cluster-zone" {{
  host_name = {q(name)}
  check_command = "cluster-zone"
  vars.cluster_zone = {q(zone)}
  check_interval = 30s
  retry_interval = 15s
  groups = [ "monitoring" ]
}}''')
    write(out, "master", "nodes.conf", header + "\n\n".join(nodes) + "\n")

    write(out, "master", "heartbeats.conf", header + heartbeat_host("master", [
        ("beat-master-01", 10, "master-01"), ("beat-master-02", 10, "master-02")]))
    write(out, "ams", "heartbeats.conf", header + heartbeat_host("ams", [("beat", 30, None)]))
    write(out, "fra", "heartbeats.conf", header + heartbeat_host("fra", [
        ("beat", 30, None), ("beat-sat-fra-01", 30, "sat-fra-01"), ("beat-sat-fra-02", 30, "sat-fra-02")]))

    write(out, "master", "api-users.conf", header + API_USERS)

    write(out, "global-templates", "demo-functions.conf", header + GLOBAL_FUNCTIONS)
    command_blocks = [HOST_COMMAND]
    for command, services in sorted(commands.items()):
        texts = "{ " + ", ".join(f"{q(name)} = {dsl(list(t) + [UNKNOWN])}" for name, t in sorted(services.items())) + " }"
        command_blocks.append(f'''object CheckCommand {q(command)} {{
  import "demo-check"
  vars.demo_texts = {texts}
}}''')
    write(out, "global-templates", "commands.conf", header + COMMAND_TEMPLATE + "\n\n".join(command_blocks) + "\n")
    write(out, "global-templates", "groups.conf", header + "\n".join(
        [f"object HostGroup {q(name)} {{ display_name = {q(title)} }}" for name, title in GROUP_NAMES.items()]
        + [f"object ServiceGroup {q(name)} {{ display_name = {q(title)} }}" for name, title in SERVICE_GROUP_NAMES.items()]) + "\n")
    write(out, "global-templates", "templates.conf", header + TEMPLATES)
    write(out, "global-templates", "users.conf", header + users())
    write(out, "global-templates", "notifications.conf", header + NOTIFICATIONS)
    write(out, "global-templates", "apply.conf", header + APPLY)
    write(out, "director-global", "service-templates.conf", header + SERVICE_TEMPLATES)


def heartbeat_host(zone, beats):
    """B3 (PLAN.md): a host in the zone, a beat per satellite zone and one
    pinned to each endpoint of an HA zone."""
    lines = [f'''/* icygui's heartbeats for zone {zone} (docs/user-guide.md, Heartbeats). */
object Host "icygui-hb-{zone}" {{
  check_command = "dummy"
  check_interval = 300s
  max_check_attempts = 1
  vars.dummy_text = "icygui heartbeat host"
}}''']
    for name, interval, endpoint in beats:
        pinned = f'\n  command_endpoint = "{endpoint}"' if endpoint else ""
        lines.append(f'''object Service "{name}" {{
  host_name = "icygui-hb-{zone}"
  check_command = "dummy"
  check_interval = {interval}s
  retry_interval = {interval}s
  max_check_attempts = 1{pinned}
  vars.dummy_state = 0
  vars.dummy_text = {{{{ "icygui heartbeat " + get_time() }}}}
  vars.icygui_heartbeat = true
}}''')
    return "\n\n".join(lines) + "\n"


COMMAND_TEMPLATE = '''/* Every check of the demo is Icinga's dummy check under the plugin's name;
 * its state and output follow the object's schedule (demo-functions.conf). */
template CheckCommand "demo-check" {
  import "dummy-check-command"
  vars.dummy_state = {{
    demo_state(get_service(macro("$host.name$"), macro("$service.name$")).vars.demo)
  }}
  vars.dummy_text = {{
    var service = get_service(macro("$host.name$"), macro("$service.name$"))
    demo_output(service.vars.demo, macro("$demo_texts$")[service.name])
  }}
}

'''

HOST_COMMAND = '''object CheckCommand "hostalive" {
  import "dummy-check-command"
  vars.dummy_state = {{ demo_state(get_host(macro("$host.name$")).vars.demo) }}
  vars.dummy_text = {{
    demo_output(get_host(macro("$host.name$")).vars.demo, [
      "PING OK - Packet loss = 0%, RTA = 0.42 ms|rta=0.42ms;3000;5000;0 pl=0%;80;100;0",
      "PING WARNING - Packet loss = 40%, RTA = 812.00 ms",
      "PING CRITICAL - Packet loss = 100%|rta=0ms;3000;5000;0 pl=100%;80;100;0",
      "PING CRITICAL - Packet loss = 100%|rta=0ms;3000;5000;0 pl=100%;80;100;0",
    ])
  }}
}'''

TEMPLATES = '''template Host "demo-host" {
  check_command = "hostalive"
  check_interval = 60s
  retry_interval = 30s
  max_check_attempts = 3
}

object TimePeriod "24x7" {
  display_name = "Always"
  ranges = {
    monday = "00:00-24:00"
    tuesday = "00:00-24:00"
    wednesday = "00:00-24:00"
    thursday = "00:00-24:00"
    friday = "00:00-24:00"
    saturday = "00:00-24:00"
    sunday = "00:00-24:00"
  }
}

object TimePeriod "workhours" {
  display_name = "Office hours"
  ranges = {
    monday = "08:00-18:00"
    tuesday = "08:00-18:00"
    wednesday = "08:00-18:00"
    thursday = "08:00-18:00"
    friday = "08:00-18:00"
  }
}
'''

SERVICE_TEMPLATES = '''/* Templates as Icinga Director would deploy them. */
template Service "demo-service" {
  max_check_attempts = 3
  enable_flapping = false
}
'''


def users():
    lines = []
    for name, display in USERS:
        groups = [group for group, members in USER_GROUPS.items() if name in members]
        lines.append(f"object User {q(name)} {{\n  display_name = {q(display)}\n"
                     f"  email = {q(name + '@example.com')}\n  groups = {dsl(groups)}\n}}")
    for name in USER_GROUPS:
        lines.append(f"object UserGroup {q(name)} {{ display_name = {q(name + ' team')} }}")
    return "\n".join(lines) + "\n"


NOTIFICATIONS = '''/* Who Icinga notifies. The commands send nothing: this is a demo. */
object NotificationCommand "mail-host-notification" { command = [ "/bin/true" ] }
object NotificationCommand "mail-service-notification" { command = [ "/bin/true" ] }

apply Notification "mail-oncall" to Host {
  command = "mail-host-notification"
  users = [ "infra-oncall" ]
  period = "24x7"
  interval = 30m
  types = [ Problem, Acknowledgement, Recovery, DowntimeStart, DowntimeEnd, FlappingStart, FlappingEnd ]
  states = [ Up, Down ]
  assign where host.vars.env == "prod" && host.vars.demo
}

apply Notification "mail-team" to Service {
  command = "mail-service-notification"
  user_groups = [ host.vars.team ]
  period = "24x7"
  interval = 30m
  types = [ Problem, Acknowledgement, Recovery, DowntimeStart, DowntimeEnd, FlappingStart, FlappingEnd ]
  states = [ OK, Warning, Critical, Unknown ]
  assign where host.vars.env == "prod" && service.vars.demo
}

apply Notification "sms-dba" to Service {
  command = "mail-service-notification"
  users = [ "dba-oncall" ]
  period = "24x7"
  interval = 0
  types = [ Problem, Recovery ]
  states = [ OK, Critical ]
  assign where "replication" in service.groups && host.vars.env == "prod"
}
'''

APPLY = '''/* Hosts at a site are reached through its core switch. */
apply Dependency "uplink" to Host {
  parent_host_name = "sw-core-" + host.vars.site + "-01"
  disable_notifications = true
  assign where host.vars.site in [ "ams", "fra" ] && host.vars.role != "switch" && host.vars.demo
}

/* The nightly backup window. One key per day: Icinga 2.15 reads the range
 * "monday - sunday" as empty (Sunday is day 0) and creates nothing. */
apply ScheduledDowntime "backup-window" to Service {
  author = "icingaadmin"
  comment = "nightly backup window"
  fixed = true
  ranges = {
    monday = "01:00-03:00"
    tuesday = "01:00-03:00"
    wednesday = "01:00-03:00"
    thursday = "01:00-03:00"
    friday = "01:00-03:00"
    saturday = "01:00-03:00"
    sunday = "01:00-03:00"
  }
  assign where service.name in [ "borg-last-run", "restic-check", "pg-backup-age" ] && host.vars.env == "prod"
}

/* Frankfurt's Kubernetes nodes update themselves every four hours, so the
 * next window from the configuration is never far away (Icinga creates only
 * the running or the next segment of a ScheduledDowntime). */
apply ScheduledDowntime "node-update-window" to Service {
  author = "icingaadmin"
  comment = "node auto-update window"
  fixed = true
  ranges = {
    monday = "02:10-02:30,06:10-06:30,10:10-10:30,14:10-14:30,18:10-18:30,22:10-22:30"
    tuesday = "02:10-02:30,06:10-06:30,10:10-10:30,14:10-14:30,18:10-18:30,22:10-22:30"
    wednesday = "02:10-02:30,06:10-06:30,10:10-10:30,14:10-14:30,18:10-18:30,22:10-22:30"
    thursday = "02:10-02:30,06:10-06:30,10:10-10:30,14:10-14:30,18:10-18:30,22:10-22:30"
    friday = "02:10-02:30,06:10-06:30,10:10-10:30,14:10-14:30,18:10-18:30,22:10-22:30"
    saturday = "02:10-02:30,06:10-06:30,10:10-10:30,14:10-14:30,18:10-18:30,22:10-22:30"
    sunday = "02:10-02:30,06:10-06:30,10:10-10:30,14:10-14:30,18:10-18:30,22:10-22:30"
  }
  assign where service.name == "kubelet" && match("k8s-fra-node-0*", host.name)
}
'''

API_USERS = '''/* The demo's API users. Their passwords are demo values, documented in
 * docs/demo.md: this Icinga only listens on 127.0.0.1 and holds nothing. */

/* What docs/user-guide.md recommends for icygui (without the optional
 * filter-expression and the opt-in execute-command). */
object ApiUser "icygui-demo" {
  password = "icygui-demo-password"
  permissions = [
    "objects/query/Host",
    "objects/query/Service",
    "objects/query/HostGroup",
    "objects/query/ServiceGroup",
    "objects/query/Comment",
    "objects/query/Downtime",
    "objects/query/Notification",
    "objects/query/Dependency",
    "objects/query/Endpoint",
    "objects/query/Zone",
    "status/query",
    "events/CheckResult",
    "events/StateChange",
    "events/Flapping",
    "events/AcknowledgementSet",
    "events/AcknowledgementCleared",
    "events/CommentAdded",
    "events/CommentRemoved",
    "events/DowntimeAdded",
    "events/DowntimeRemoved",
    "events/DowntimeStarted",
    "events/DowntimeTriggered",
    "events/ObjectCreated",
    "events/ObjectModified",
    "events/ObjectDeleted",
    "events/Notification",
    "actions/reschedule-check",
    "actions/acknowledge-problem",
    "actions/remove-acknowledgement",
    "actions/schedule-downtime",
    "actions/remove-downtime",
    "actions/add-comment",
    "actions/remove-comment",
    "actions/process-check-result",
  ]
}

/* For the seed (seed.py) and the scenarios (demo/scenario.sh) only. */
object ApiUser "demo-admin" {
  password = "demo-admin-password"
  permissions = [ "*" ]
}
'''

if __name__ == "__main__":
    if len(sys.argv) != 2:
        raise SystemExit("usage: generate.py OUT_DIR")
    main(sys.argv[1])
