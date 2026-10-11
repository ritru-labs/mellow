#!/usr/bin/env bash
# Exercise the actual package entrypoint with synthetic Docker argument capture.
# Shell/checkout regression only; never container or product acceptance.
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
export MELLOW_PACKAGE_INVOCATION_ROOT="$root"
python3 - <<'PYTHON'
import json, os, pathlib, platform, shutil, subprocess, tempfile

source = pathlib.Path(os.environ['MELLOW_PACKAGE_INVOCATION_ROOT'])
shell = '/bin/bash' if platform.system() == 'Darwin' else shutil.which('bash')
with tempfile.TemporaryDirectory(prefix='mellow-package-invocation-') as temporary:
    base = pathlib.Path(temporary).resolve()
    checkout = base / 'normal clone with spaces'
    (checkout / 'scripts').mkdir(parents=True)
    shutil.copy2(source / 'scripts/package-linux.sh', checkout / 'scripts/package-linux.sh')
    (checkout / 'Cargo.toml').write_text('[package]\nversion = "0.0.0"\n')
    (checkout / '.gitignore').write_text('dist/\n')
    def git(root, *args):
        return subprocess.check_output(['git', '-C', str(root), *args], text=True).strip()
    git(checkout, 'init', '-q')
    git(checkout, 'add', '.')
    git(checkout, '-c', 'user.name=Synthetic Package Fixture',
        '-c', 'user.email=fixture@example.invalid', 'commit', '-qm', 'Synthetic package invocation')
    sha = git(checkout, 'rev-parse', 'HEAD')
    linked = base / 'linked worktree with spaces'
    git(checkout, 'worktree', 'add', '-qb', 'fixture-linked', str(linked))
    commands = base / 'commands'
    commands.mkdir()
    docker = commands / 'docker'
    docker.write_text('#!/usr/bin/env python3\n' + '''
import json, os, pathlib, sys
args = sys.argv[1:]
with open(os.environ['MELLOW_DOCKER_CAPTURE'], 'a') as handle:
    handle.write(json.dumps(args) + '\\n')
if args[0] == 'build':
    print('synthetic-image-not-executed')
elif args[0] == 'run':
    out = pathlib.Path.cwd() / 'dist/linux'
    out.mkdir(parents=True, exist_ok=True)
    for name in ('fixture.tar.gz', 'fixture.deb', 'fixture.rpm'):
        (out / name).write_bytes(b'synthetic-argument-fixture-not-a-package')
else:
    sys.exit('Unexpected Docker operation')
''')
    docker.chmod(0o755)
    outcomes = []
    for name, root in [('normal-clone', checkout), ('linked-worktree', linked)]:
        capture = base / (name + '.jsonl')
        env = os.environ | {'PATH': str(commands) + os.pathsep + os.environ['PATH'],
                            'MELLOW_DOCKER_CAPTURE': str(capture),
                            'BUILD_URL': 'http://127.0.0.1:8082/job/synthetic-package/1/'}
        result = subprocess.run([shell, 'scripts/package-linux.sh', 'aarch64'],
                                cwd=root, env=env, capture_output=True, text=True)
        assert result.returncode == 0, (name, result.stdout, result.stderr)
        calls = [json.loads(line) for line in capture.read_text().splitlines()]
        assert len(calls) == 2 and calls[0][0] == 'build' and calls[1][0] == 'run', calls
        args = calls[1]
        assert '' not in args, args
        common = (root / git(root, 'rev-parse', '--git-common-dir')).resolve()
        git_mount = str(common) + ':' + str(common) + ':ro'
        assert (git_mount in args) == (name == 'linked-worktree'), args
        assert str(root) + ':/workspace' in args, args
        assert 'MELLOW_PACKAGE_SOURCE_COMMIT=' + sha in args, args
        assert 'BUILD_URL=' + env['BUILD_URL'] in args, args
        assert git(root, 'rev-parse', 'HEAD') == sha
        assert not git(root, 'status', '--porcelain', '--untracked-files=all')
        outcomes.append({'case': name, 'shell': shell, 'passed': True,
                         'docker_execution': 'synthetic argument capture only'})
    print(json.dumps(outcomes, indent=2))
    print('2 package invocation regressions passed; no container/product acceptance claimed')
PYTHON
