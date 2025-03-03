#!/bin/bash

set -e

export DOCKER_BUILDKIT=1
DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" >/dev/null 2>&1 && pwd )"
cd "$DIR"

cat ../rust-toolchain
# shellcheck disable=SC2155
export RUST_TOOLCHAIN=$(cat ../rust-toolchain)
export BUILD_ENV_VERSION=v20220621
export BUILD_TAG="public.ecr.aws/x5u3w5h6/rw-build-env:${BUILD_ENV_VERSION}"

echo "--- Docker build"
docker build -t ${BUILD_TAG} --build-arg "RUST_TOOLCHAIN=${RUST_TOOLCHAIN}" .

echo "--- Docker login"
aws ecr-public get-login-password --region us-east-1 | docker login --username AWS --password-stdin public.ecr.aws/x5u3w5h6

echo "--- Docker push"
docker push ${BUILD_TAG}
