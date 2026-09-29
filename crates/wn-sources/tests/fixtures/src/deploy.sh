#!/usr/bin/env bash
# Deploy the release bundle to the staging hosts over ssh.
set -euo pipefail

build_bundle() {
  tar czf bundle.tgz dist
}

function push-hosts {
  for h in a b; do scp bundle.tgz "$h":; done
}
