// Mellow native gate for an existing Jenkins controller.
// Create this as a manual Pipeline job (no SCM polling). It runs the same
// shared gate as GitHub Actions, scripts/native-ci.sh verify, inside a pinned
// Rust 1.90.0 container. No extra Jenkins plugin is needed: only Docker.
// The PTY smoke tests open their own terminal (openpty), so the container
// does not need a TTY.
pipeline {
  agent any
  options {
    disableConcurrentBuilds()
    skipDefaultCheckout(true)
    timestamps()
    timeout(time: 60, unit: 'MINUTES')
    buildDiscarder(logRotator(numToKeepStr: '10', artifactNumToKeepStr: '5'))
  }
  parameters {
    string(name: 'SOURCE_SHA', defaultValue: '', trim: true, description: 'Full 40-character commit ID; empty means the branch head')
    string(name: 'SOURCE_REPOSITORY', defaultValue: 'https://github.com/ritru-labs/mellow.git', description: 'Public repository to clone over HTTPS')
    string(name: 'SOURCE_BRANCH', defaultValue: 'feature/agent-style-ai-ui', description: 'Branch to build when SOURCE_SHA is empty')
  }
  stages {
    stage('Validate') {
      steps {
        script {
          if (params.SOURCE_SHA && !(params.SOURCE_SHA ==~ /[0-9a-f]{40}/)) {
            error 'SOURCE_SHA must be a full 40-character lowercase commit ID'
          }
        }
      }
    }
    stage('Verify') {
      steps {
        deleteDir()
        checkout([$class: 'GitSCM',
          branches: [[name: params.SOURCE_SHA ?: "*/${params.SOURCE_BRANCH}"]],
          userRemoteConfigs: [[url: params.SOURCE_REPOSITORY]],
          extensions: [[$class: 'CleanBeforeCheckout']]])
        sh '''
          docker run --rm \
            -v "$WORKSPACE:/src" -w /src \
            rust:1.90.0 bash scripts/native-ci.sh verify
        '''
      }
    }
  }
  post {
    always {
      echo "Mellow native gate finished: ${currentBuild.currentResult}"
    }
  }
}
