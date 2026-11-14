#!/bin/sh
set -eu

out=${1:?usage: prepare-certificates.sh OUTPUT_DIRECTORY}
mkdir -p "$out"
cat >"$out/ca.cnf" <<'EOF'
[req]
distinguished_name = dn
x509_extensions = ca
prompt = no
[dn]
CN = bctl WebMCP fixture CA
[ca]
basicConstraints = critical,CA:TRUE
keyUsage = critical,keyCertSign,cRLSign
subjectKeyIdentifier = hash
EOF
cat >"$out/server.ext" <<'EOF'
subjectAltName = IP:100.96.0.1,IP:127.0.0.1
basicConstraints = critical,CA:FALSE
keyUsage = critical,digitalSignature,keyEncipherment
extendedKeyUsage = serverAuth
EOF
umask 077
openssl req -x509 -newkey rsa:2048 -nodes -days 2 -sha256 -config "$out/ca.cnf" \
    -keyout "$out/ca.key" -out "$out/ca.crt"
openssl req -new -newkey rsa:2048 -nodes -subj '/CN=bctl WebMCP fixture' \
    -keyout "$out/server.key" -out "$out/server.csr"
openssl x509 -req -days 2 -sha256 -in "$out/server.csr" -CA "$out/ca.crt" \
    -CAkey "$out/ca.key" -CAserial "$out/ca.srl" -CAcreateserial -extfile "$out/server.ext" -out "$out/server.crt"
openssl verify -CAfile "$out/ca.crt" "$out/server.crt"
