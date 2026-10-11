#!/usr/bin/env bash
# Synthetic clean repositories prove the acceptance boundary, not product gates.
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
export MELLOW_JENKINS_HELPER_ROOT="$root"
export RUST_TOOLCHAIN_BIN="${RUST_TOOLCHAIN_BIN:-/Users/rukmini/.rustup/toolchains/1.90.0-aarch64-apple-darwin/bin}"
python3 - <<'PY'
import json, os, pathlib, platform, shutil, subprocess, tempfile

root = pathlib.Path(os.environ['MELLOW_JENKINS_HELPER_ROOT'])
targets = {('Darwin', 'arm64'): 'aarch64-apple-darwin',
           ('Darwin', 'x86_64'): 'x86_64-apple-darwin',
           ('Linux', 'aarch64'): 'aarch64-unknown-linux-musl',
           ('Linux', 'x86_64'): 'x86_64-unknown-linux-musl'}
native = targets[(platform.system(), platform.machine())]
with tempfile.TemporaryDirectory(prefix='mellow-jenkins-preflight-') as temporary:
    checkout = pathlib.Path(temporary, 'source')
    (checkout / 'scripts').mkdir(parents=True)
    shutil.copy(root / 'scripts/jenkins-candidate.sh', checkout / 'scripts/jenkins-candidate.sh')
    shutil.copy(root / 'Jenkinsfile', checkout / 'Jenkinsfile')
    (checkout / '.gitignore').write_text('dist/\n')
    def git(*args):
        return subprocess.check_output(['git', '-C', str(checkout), *args], text=True).strip()
    git('init', '-q')
    git('add', '.')
    git('-c', 'user.name=Synthetic CI Fixture', '-c', 'user.email=fixture@example.invalid',
        'commit', '-qm', 'Synthetic preflight boundary')
    sha = git('rev-parse', 'HEAD')
    env = os.environ | {'SOURCE_SHA': sha, 'EXPECTED_TARGET': native}
    outcomes = []
    def run(name, changes, expected):
        result = subprocess.run(['bash', 'scripts/jenkins-candidate.sh', 'preflight'],
                                cwd=checkout, env=env | changes, capture_output=True, text=True)
        assert result.returncode == expected, (name, result.stdout, result.stderr)
        receipt = (checkout / 'dist/jenkins-evidence/result.txt').read_text()
        assert 'candidate_acceptance=passed' not in receipt
        assert ('preflight=passed' in receipt) == (expected == 0)
        outcomes.append({'case': name, 'exit': result.returncode, 'receipt': receipt})
    run('clean-exact-sha', {}, 0)
    run('wrong-checkout-sha', {'SOURCE_SHA': '0' * 40}, 1)
    run('branch-is-not-sha', {'SOURCE_SHA': 'main'}, 1)
    run('wrong-native-target', {'EXPECTED_TARGET': 'unsupported-target'}, 1)
    (checkout / 'untracked-owner-work.txt').write_text('must remain intact')
    run('dirty-source-refused', {}, 1)
    assert (checkout / 'untracked-owner-work.txt').read_text() == 'must remain intact'
    (checkout / 'untracked-owner-work.txt').unlink()
    # Only this preflight fixture uses a synthetic rustc; it never builds Mellow.
    wrong_toolchain = pathlib.Path(temporary, 'wrong-toolchain')
    wrong_toolchain.mkdir()
    rustc = wrong_toolchain / 'rustc'
    rustc.write_text('#!/bin/sh\ncase "$1" in --version) echo "rustc 1.90.0" ;; -vV) echo "host: incompatible-native-host" ;; esac\n')
    rustc.chmod(0o755)
    cargo = wrong_toolchain / 'cargo'
    cargo.write_text('#!/bin/sh\nexit 99\n')
    cargo.chmod(0o755)
    run('wrong-toolchain-host', {'RUST_TOOLCHAIN_BIN': str(wrong_toolchain)}, 1)
    assert git('rev-parse', 'HEAD') == sha
    assert not git('status', '--porcelain', '--untracked-files=all')
    destination = root / 'dist/jenkins-preflight-regressions.json'
    destination.parent.mkdir(exist_ok=True)
    destination.write_text(json.dumps(outcomes, indent=2) + '\n')
    print('6 synthetic Jenkins preflight boundary cases passed; no product acceptance claimed')
PY
