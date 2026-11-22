#!/usr/bin/env bash
set -euo pipefail

PG_CONT="apsis-e2e-pg-$$"
MONGO_CONT="apsis-e2e-mongo-$$"
S3_CONT="apsis-e2e-s3-$$"

cleanup() {
  podman rm -f "$PG_CONT" "$MONGO_CONT" "$S3_CONT" >/dev/null 2>&1 || true
}
trap cleanup EXIT

echo "Starting PostgreSQL, MongoDB, and S3Mock containers..."
podman run -d --rm --name "$PG_CONT" \
  -p 15432:5432 \
  -e POSTGRES_USER=apsis \
  -e POSTGRES_PASSWORD=apsis_pw \
  -e POSTGRES_DB=apsis_test \
  docker.io/library/postgres:17-alpine >/dev/null

podman run -d --rm --name "$MONGO_CONT" \
  -p 27117:27017 \
  docker.io/library/mongo:8.0 >/dev/null

podman run -d --rm --name "$S3_CONT" \
  -p 19090:9090 \
  -e COM_ADOBE_TESTING_S3MOCK_STORE_INITIAL_BUCKETS=apsis-test \
  -e initialBuckets=apsis-test \
  docker.io/adobe/s3mock:latest >/dev/null

echo "Waiting for PostgreSQL..."
for _ in $(seq 1 40); do
  if podman exec "$PG_CONT" pg_isready -U apsis -d apsis_test >/dev/null 2>&1; then
    break
  fi
  sleep 0.5
done

echo "Waiting for MongoDB..."
for _ in $(seq 1 40); do
  if podman exec "$MONGO_CONT" mongosh --quiet --eval "db.runCommand({ ping: 1 }).ok" >/dev/null 2>&1; then
    break
  fi
  sleep 0.5
done

echo "Waiting for S3Mock..."
for _ in $(seq 1 60); do
  if curl -sf http://127.0.0.1:19090/apsis-test >/dev/null 2>&1 || [ "$(curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:19090/)" != "000" ]; then
    break
  fi
  sleep 0.5
done

cargo test --test real_connectors -- --ignored --nocapture
