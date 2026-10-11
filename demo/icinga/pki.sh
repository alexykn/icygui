#!/bin/sh
# One-shot: the demo cluster's certificates from Icinga's own CA
# (`icinga2 pki`), once per fresh `ca` volume. Started as root by the
# `pki` service (a new volume belongs to root), it hands the volumes to the
# icinga user (5665) and runs `icinga2 pki` as that user, which is what
# the CLI expects. Each node finds its key, certificate and the CA
# certificate in /pki/<node>/; the CA itself (with its key) goes to the
# `ca` volume, which only master-01, the config master, mounts.
set -eu

NODES="master-01 master-02 sat-ams-01 sat-fra-01 sat-fra-02"

if [ -f /ca/ca.crt ]; then
  echo "pki: the CA exists, nothing to do"
  exit 0
fi

if [ "$(id -u)" = 0 ]; then
  # `icinga2 pki new-ca` and `sign-csr` work on /var/lib/icinga2/ca,
  # which the image links into /data.
  mkdir -p /data/var/lib/icinga2 /data/var/log/icinga2 /data/var/run/icinga2 /data/var/cache/icinga2
  chown -R 5665:5665 /data /pki /ca
  exec su -s /bin/sh icinga -c "sh $0"
fi

# A `pki` volume from before the CA had a volume of its own: its CA (and
# key) goes, the nodes get new certificates below.
rm -rf /pki/ca

icinga2 pki new-ca
cp /var/lib/icinga2/ca/ca.crt /var/lib/icinga2/ca/ca.key /ca/
chmod 700 /ca

for node in $NODES; do
  rm -rf "/pki/$node"
  mkdir -p "/pki/$node"
  icinga2 pki new-cert --cn "$node" --key "/pki/$node/$node.key" --csr "/pki/$node/$node.csr"
  icinga2 pki sign-csr --csr "/pki/$node/$node.csr" --cert "/pki/$node/$node.crt"
  cp /ca/ca.crt "/pki/$node/ca.crt"
done
echo "pki: a CA and certificates for $NODES"
