// Mellow native gate for an existing Jenkins controller.
// Create this as a manual Pipeline job (no SCM polling). It runs the same
// shared gate as GitHub Actions: scripts/native-ci.sh verify.
pipeline {
  agent none
  options {
    disableConcurrentBuilds()
    skipDefaultCheckout(true)
    timestamps()
    timeout(time: 60, unit: 'MINUTES')
    buildDiscarder(logRotator(numToKeepStr: '10', artifactNumToKeepStr: '5'))
  }
  parameters {
    string(name: 'SOURCE_SHA', defaultValue: '', trim: true, description: 'Required full 40-character commit ID')
    string(name: 'SOURCE_REPOSITORY', defaultValue: 'https://github.com/ritru-labs/mellow.git', description: 'Repository URL to clone')
    string(name: 'AGENT_LABEL', defaultValue: 'linux-x64', description: 'Jenkins agent label with Rust 1.90.0 and a PTY')
  }
  stages {
    stage('Validate') {
      steps {
        script {
          if (!(params.SOURCE_SHA ==~ /[0-9a-f]{40}/)) {
            error 'SOURCE_SHA must be a full 40-character lowercase commit ID'
          }
        }
      }
    }
    stage('Verify') {
      agent { label params.AGENT_LABEL }
      steps {
        deleteDir()
        checkout([$class: 'GitSCM',
          branches: [[name: params.SOURCE_SHA]],
          userRemoteConfigs: [[url: params.SOURCE_REPOSITORY]],
          extensions: [[$class: 'CleanBeforeCheckout']]])
        sh 'bash scripts/native-ci.sh verify'
      }
    }
  }
  post {
    always {
      echo "Mellow native gate finished: ${currentBuild.currentResult}"
    }
  }
}
