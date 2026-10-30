#!/usr/bin/env bash
# Test SSH server for the SSH tunnel tests (db/tests/ssh.rs). Run on the test box, next to the
# test databases (containers telecat-test-mysql and telecat-test-pg):
#
#   scripts/test-ssh-env.sh          create or refresh everything (idempotent)
#   scripts/test-ssh-env.sh down     remove the SSH server, its network and its keys
#
# What it sets up:
# - Docker network telecat-test-net with both test database containers attached, so the SSH
#   server reaches them by name: telecat-test-mysql:3306 and telecat-test-pg:5432.
# - Container telecat-test-sshd (Debian + OpenSSH), published only on 127.0.0.1:2222. It only
#   forwards to the two test databases (PermitOpen), never to anything else.
# - Users: tpass (password "sshsecret"), tkey (public keys only), tkbd (keyboard-interactive
#   only, password "sshsecret").
# - Client keys generated here, never committed: $TELECAT_SSH_TEST_DIR (default
#   /root/telecat/ssh-test). Encrypted ones use the passphrase "keypass".
#   id_ed25519, id_ed25519_enc, id_rsa (OpenSSH), id_rsa_pem (PKCS#1), id_rsa_pem_enc,
#   id_rsa_pkcs8, id_ecdsa (OpenSSH), id_ecdsa_pem (SEC1), id_unauthorized (not authorized),
#   plus sshd_ip (the server's address on the network, for the tests' server-side checks).
set -euo pipefail

DIR="${TELECAT_SSH_TEST_DIR:-/root/telecat/ssh-test}"
NET=telecat-test-net
NAME=telecat-test-sshd
IMAGE=telecat-test-sshd:1
DATABASES=(telecat-test-mysql telecat-test-pg)

if [[ "${1:-}" == "down" ]]; then
  docker rm -f "$NAME" >/dev/null 2>&1 || true
  for db in "${DATABASES[@]}"; do docker network disconnect "$NET" "$db" >/dev/null 2>&1 || true; done
  docker network rm "$NET" >/dev/null 2>&1 || true
  rm -rf "$DIR"
  echo "SSH test environment removed."
  exit 0
fi

for db in "${DATABASES[@]}"; do
  docker inspect "$db" >/dev/null 2>&1 || { echo "Missing test database container $db" >&2; exit 1; }
done
docker network inspect "$NET" >/dev/null 2>&1 || docker network create "$NET" >/dev/null
for db in "${DATABASES[@]}"; do
  if ! docker inspect -f '{{json .NetworkSettings.Networks}}' "$db" | grep -q "\"$NET\""; then
    docker network connect "$NET" "$db"
  fi
done

mkdir -p "$DIR"
chmod 700 "$DIR"
key() { # name, ssh-keygen arguments…
  local name=$1; shift
  [[ -f "$DIR/$name" ]] || ssh-keygen -q -C "telecat-test-$name" -f "$DIR/$name" "$@"
}
key id_ed25519 -t ed25519 -N ""
key id_ed25519_enc -t ed25519 -N keypass
key id_rsa -t rsa -b 3072 -N ""
key id_rsa_pem -t rsa -b 3072 -m PEM -N ""
key id_rsa_pem_enc -t rsa -b 3072 -m PEM -N keypass
key id_rsa_pkcs8 -t rsa -b 3072 -m PKCS8 -N ""
key id_ecdsa -t ecdsa -b 256 -N ""
key id_ecdsa_pem -t ecdsa -b 256 -m PEM -N ""
key id_unauthorized -t ed25519 -N ""
cat "$DIR"/id_{ed25519,ed25519_enc,rsa,rsa_pem,rsa_pem_enc,rsa_pkcs8,ecdsa,ecdsa_pem}.pub > "$DIR/authorized_keys"

BUILD=$(mktemp -d)
trap 'rm -rf "$BUILD"' EXIT
cat > "$BUILD/Dockerfile" <<'DOCKERFILE'
FROM debian:bookworm-slim
RUN apt-get update && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends openssh-server \
 && rm -rf /var/lib/apt/lists/* && mkdir -p /run/sshd && ssh-keygen -A \
 && useradd -m -s /bin/bash tpass && echo 'tpass:sshsecret' | chpasswd \
 && useradd -m -s /bin/bash tkey && passwd -d tkey && mkdir -p /home/tkey/.ssh \
 && useradd -m -s /bin/bash tkbd && echo 'tkbd:sshsecret' | chpasswd
COPY sshd_config /etc/ssh/sshd_config
COPY authorized_keys /home/tkey/.ssh/authorized_keys
RUN chown -R tkey:tkey /home/tkey/.ssh && chmod 700 /home/tkey/.ssh && chmod 600 /home/tkey/.ssh/authorized_keys
EXPOSE 22
CMD ["/usr/sbin/sshd", "-D", "-e"]
DOCKERFILE
cat > "$BUILD/sshd_config" <<'SSHD'
Port 22
PermitRootLogin no
UsePAM yes
PubkeyAuthentication yes
PasswordAuthentication yes
KbdInteractiveAuthentication no
PermitEmptyPasswords no
AllowTcpForwarding local
PermitOpen telecat-test-mysql:3306 telecat-test-pg:5432
X11Forwarding no
AllowAgentForwarding no
PermitTunnel no
MaxAuthTries 6
Subsystem sftp internal-sftp
Match User tkey
    PasswordAuthentication no
Match User tkbd
    PasswordAuthentication no
    KbdInteractiveAuthentication yes
SSHD
cp "$DIR/authorized_keys" "$BUILD/authorized_keys"
docker build -q -t "$IMAGE" "$BUILD" >/dev/null

docker rm -f "$NAME" >/dev/null 2>&1 || true
docker run -d --name "$NAME" --network "$NET" -p 127.0.0.1:2222:22 --restart unless-stopped "$IMAGE" >/dev/null
docker inspect -f "{{(index .NetworkSettings.Networks \"$NET\").IPAddress}}" "$NAME" > "$DIR/sshd_ip"
for _ in $(seq 1 50); do
  if ssh-keyscan -p 2222 127.0.0.1 >/dev/null 2>&1; then
    echo "SSH test server ready on 127.0.0.1:2222 (address on $NET: $(cat "$DIR/sshd_ip")); keys in $DIR"
    exit 0
  fi
  sleep 0.2
done
echo "The SSH test server didn't start; see: docker logs $NAME" >&2
exit 1
