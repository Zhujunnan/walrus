#!/bin/bash
# export DOCKER_DEFAULT_PLATFORM=linux/amd64

msg() {
  echo "$0: note: $*" >&2
}

die() {
  echo "$0: $*" >&2
  exit 1
}

# chdir to git root
git_root=$(cd "$(dirname "$0")" && git rev-parse --show-toplevel) || die "Failed to get git root"
cd "$git_root" || die "Failed to chdir to git root"

build_dir="$(realpath "$(dirname "$0")")"
sui_version="$(cargo tree --package sui-rpc-api | grep sui-rpc-api | grep -Eo 'testnet-[^#)]*')"
msg "Using SUI version: $sui_version"
# Manually start local registry.
# docker run -d -p 5000:5000 --restart always --name registry registry:2
# TODO: check that local registry is running.

local_sui_image_name=sui-tools:"$sui_version"

if false; then
(
  # Assume SUI is in ../sui.
  cd ../sui || die "no sui dir?"
  git fetch origin || die "Failed to fetch SUI"
  git checkout "$sui_version" || die "Failed to checkout SUI version '$sui_version'"
  cd docker/sui-tools
  ./build.sh -t "$local_sui_image_name" || die "Failed to build SUI image"
  # Get SUI image and push to local registry.
) || die "failed to build SUI image"
fi

local_walrus_image=walrus-service:pseudo-local-antithesis

export WALRUS_IMAGE_NAME="$local_walrus_image"
export SUI_IMAGE_NAME="mysten/sui-tools:$sui_version"

# shellcheck disable=SC2155
# export SUI_PLATFORM=linux/"$(uname -m)"
# shellcheck disable=SC2155
export WALRUS_PLATFORM=linux/"$(uname -m)"

# Build walrus-service image.
msg "Running walrus-service build"
docker/walrus-service/build.sh -t "$local_walrus_image" || die "Failed to build walrus-service image"

msg "Running docker compose"
cd "$build_dir" || die "Failed to chdir to build dir"
docker compose \
  --env-file <(
    echo WALRUS_IMAGE_NAME="$WALRUS_IMAGE_NAME"
    echo SUI_IMAGE_NAME="$SUI_IMAGE_NAME"
  ) \
  -f "$build_dir"/docker-compose.yaml \
  up \
    --pull never \
    --force-recreate \
    --abort-on-container-failure
