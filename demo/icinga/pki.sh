#!/bin/sh
# One-shot: the demo cluster's certificates from Icinga's own CA
# (`icinga2 pki`), once per fresh `pki` volume. Started as root by the
# `pki` service (a new volume belongs to root), it hands the volume to the
# icinga user (5665) and runs `icinga2 pki` as that user, which is what
# the CLI expects. Each node finds its key, certificate and the CA
# certificate in /pki/<node>/; the CA itself stays in /pki/ca.
set -eu

NODES="master-01 master-02 sat-ams-01 sat-fra-01 sat-fra-02"

if [ -f /pki/ca/ca.crt ]; then
  echo "pki: the CA exists, nothing to do"
  exit 0
fi

if [ "$(id -u)" = 0 ]; then
  # `icinga2 pki new-ca` and `sign-csr` work on /var/lib/icinga2/ca,
  # which the image links into /data.
  mkdir -p /data/var/lib/icinga2 /data/var/log/icinga2 /data/var/run/icinga2 /data/var/cache/icinga2
  chown -R 5665:5665 /data /pki
  exec su -s /bin/sh icinga -c "sh $0"
fi

icinga2 pki new-ca
mkdir -p /pki/ca
cp /var/lib/icinga2/ca/ca.crt /var/lib/icinga2/ca/ca.key /pki/ca/
chmod 700 /pki/ca

for node in $NODES; do
  mkdir -p "/pki/$node"
  icinga2 pki new-cert --cn "$node" --key "/pki/$node/$node.key" --csr "/pki/$node/$node.csr"
  icinga2 pki sign-csr --csr "/pki/$node/$node.csr" --cert "/pki/$node/$node.crt"
  cp /pki/ca/ca.crt "/pki/$node/ca.crt"
done
echo "pki: a CA and certificates for $NODES"
