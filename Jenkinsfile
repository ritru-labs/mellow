// Manual candidate acceptance on existing local agents; no publishing steps.
pipeline {
  agent none
  options {
    skipDefaultCheckout(true)
    disableConcurrentBuilds()
    timestamps()
    timeout(time: 240, unit: 'MINUTES')
    buildDiscarder(logRotator(numToKeepStr: '10', artifactNumToKeepStr: '5'))
  }
  parameters {
    string(name: 'SOURCE_SHA', defaultValue: '', trim: true, description: 'Required immutable 40-character commit ID; never a branch or tag')
    string(name: 'SOURCE_REPOSITORY', defaultValue: 'https://github.com/ritru-labs/mellow.git', description: 'Public Mellow repository or absolute agent-local repository path')
    string(name: 'JENKINS_BASE_URL', defaultValue: 'http://127.0.0.1:8082/', description: 'Evidence URL fallback when this controller has no configured root URL; include the trailing slash')
    string(name: 'MAC_ARM_LABEL', defaultValue: 'vidhur-mac && darwin-arm64', description: 'Existing native macOS ARM64 agent')
    booleanParam(name: 'RUN_MAC_INTEL', defaultValue: false, description: 'Require an existing native Intel Mac agent; unavailable agent fails within the platform timeout')
    string(name: 'MAC_INTEL_LABEL', defaultValue: 'darwin-x64', description: 'Existing native Intel Mac label; no cross-build acceptance')
    booleanParam(name: 'RUN_LINUX_NATIVE', defaultValue: false, description: 'Require an existing native Linux agent; the Docker controller is not a native agent')
    string(name: 'LINUX_LABEL', defaultValue: 'linux-arm64', description: 'Existing native Linux agent label')
    choice(name: 'LINUX_TARGET', choices: ['aarch64-unknown-linux-musl', 'x86_64-unknown-linux-musl'], description: 'Must match the actual native Linux agent architecture')
    booleanParam(name: 'RUN_LINUX_VM_PACKAGES', defaultValue: false, description: 'Optional ARM64 Linux Docker VM packages on Mac; explicitly not native Linux acceptance')
    string(name: 'MAC_ARM_RUST_BIN', defaultValue: '/Users/rukmini/.rustup/toolchains/1.90.0-aarch64-apple-darwin/bin', description: 'Existing Rust 1.90.0 toolchain; added to PATH for this build only')
    string(name: 'TARGET_CACHE_BASE', defaultValue: '/Users/rukmini/.cache/mellow-jenkins', description: 'Agent-owned compiler cache; keyed by native target and Rust version, never source or acceptance receipts')
    string(name: 'MAC_INTEL_RUST_BIN', defaultValue: '', description: 'Existing native Intel Rust 1.90.0 bin directory when requested')
    string(name: 'LINUX_RUST_BIN', defaultValue: '', description: 'Existing native Linux Rust 1.90.0 bin directory when requested')
  }
  stages {
    stage('Exact-source candidate acceptance') {
      steps {
        script {
          if (!(params.SOURCE_SHA ==~ /[0-9a-f]{40}/)) {
            error('SOURCE_SHA must be a full immutable lowercase commit ID')
          }
          def evidenceUrl = env.BUILD_URL ?: "${params.JENKINS_BASE_URL}job/${env.JOB_NAME.replace('/', '/job/')}/${env.BUILD_NUMBER}/"
          def platforms = [[name: 'mac-arm64', target: 'aarch64-apple-darwin', label: params.MAC_ARM_LABEL, rust: params.MAC_ARM_RUST_BIN]]
          if (params.RUN_MAC_INTEL) {
            platforms.add([name: 'mac-intel', target: 'x86_64-apple-darwin', label: params.MAC_INTEL_LABEL, rust: params.MAC_INTEL_RUST_BIN])
          }
          if (params.RUN_LINUX_NATIVE) {
            platforms.add([name: 'linux-native', target: params.LINUX_TARGET, label: params.LINUX_LABEL, rust: params.LINUX_RUST_BIN])
          }
          for (def platform : platforms) {
            stage(platform.name) {
              catchError(buildResult: 'FAILURE', stageResult: 'FAILURE') {
                // Includes queue time: unavailable labels never count as passing.
                timeout(time: 90, unit: 'MINUTES') {
                  node(platform.label) {
                    ws("${env.WORKSPACE}/mellow-candidate-${env.BUILD_NUMBER}-${platform.name}") {
                      try {
                        withEnv(["SOURCE_SHA=${params.SOURCE_SHA}", "SOURCE_REPOSITORY=${params.SOURCE_REPOSITORY}", "BUILD_URL=${evidenceUrl}", "EXPECTED_TARGET=${platform.target}", "RUST_TOOLCHAIN_BIN=${platform.rust}", "TARGET_CACHE_BASE=${params.TARGET_CACHE_BASE}", "RUN_LINUX_VM_PACKAGES=${platform.name == 'mac-arm64' && params.RUN_LINUX_VM_PACKAGES}", 'GIT_TERMINAL_PROMPT=0']) {
                          sh '''#!/usr/bin/env bash
set -euo pipefail
mkdir -p evidence
exec > >(tee -a evidence/bootstrap.log) 2>&1
export PATH="/opt/homebrew/bin:/usr/local/bin:$PATH"
case "$SOURCE_REPOSITORY" in
  https://github.com/ritru-labs/mellow|https://github.com/ritru-labs/mellow.git|/*) ;;
  *) echo 'Use the public Mellow repository or an absolute agent-local path' >&2; exit 1 ;;
esac
[[ ! -e source ]] || { echo 'Isolated source directory already exists; refusing to overwrite it' >&2; exit 1; }
{
  git init source
  git -C source remote add origin "$SOURCE_REPOSITORY"
  git -C source fetch --no-tags --depth=1 origin "$SOURCE_SHA"
  git -C source checkout --detach "$SOURCE_SHA"
  [[ "$(git -C source rev-parse HEAD)" == "$SOURCE_SHA" ]]
} 2>&1 | tee evidence/checkout.log
cd source
bash scripts/jenkins-candidate.sh
'''
                        }
                      } finally {
                        archiveArtifacts(artifacts: 'evidence/**,source/dist/jenkins-evidence/**,source/dist/macos/*,source/dist/linux/*', allowEmptyArchive: true, fingerprint: true)
                      }
                    }
                  }
                }
              }
            }
          }
        }
      }
    }
  }
}
