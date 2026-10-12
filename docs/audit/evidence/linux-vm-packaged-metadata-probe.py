import os, pathlib, struct, tarfile, tempfile, hashlib, subprocess, time, select, fcntl, termios, signal, errno
archive=pathlib.Path('/artifacts/mellow-0.3.0-aarch64-unknown-linux-musl.tar.gz')
name='mellow-0.3.0-aarch64-unknown-linux-musl'
base=pathlib.Path(tempfile.mkdtemp(prefix='mellow-packaged-metadata-'))
os.chmod(base,0o755)
with tarfile.open(archive,'r:gz') as tar:
    build=tar.extractfile(name+'/BUILD.txt').read().decode()
    assert 'source_commit=917238f93ec19a7036bfd7fba777a514c8032bbf\n' in build
    assert 'target=aarch64-unknown-linux-musl\n' in build
    binary=base/'mellow'; binary.write_bytes(tar.extractfile(name+'/mellow').read()); os.chmod(binary,0o755)
print('archive_sha256='+hashlib.sha256(archive.read_bytes()).hexdigest(),flush=True)
print('binary_sha256='+hashlib.sha256(binary.read_bytes()).hexdigest(),flush=True)
print(build,flush=True)
print(subprocess.check_output(['uname','-srm'],text=True).strip(),flush=True)
print(subprocess.check_output(['file',str(binary)],text=True).strip(),flush=True)

def exercise(path, uid, wanted):
    home=base/('home-'+str(uid)); home.mkdir(mode=0o700)
    state=home/'state'/'mellow'; state.mkdir(parents=True)
    (state/'onboarding-v1.seen').write_text('seen\n')
    if uid: 
        for candidate in [home,home/'state',state,state/'onboarding-v1.seen']: os.chown(candidate,uid,uid)
    master,slave=os.openpty(); fcntl.ioctl(slave,termios.TIOCSWINSZ,struct.pack('HHHH',24,240 if uid else 80,0,0))
    pid=os.fork()
    if pid==0:
        os.close(master); os.setsid(); fcntl.ioctl(slave,termios.TIOCSCTTY,0)
        for fd in (0,1,2): os.dup2(slave,fd)
        if slave>2: os.close(slave)
        os.chdir(path.parent)
        if uid: os.setgroups([]); os.setgid(uid); os.setuid(uid)
        env={'HOME':str(home),'XDG_CONFIG_HOME':str(home/'config'),'XDG_STATE_HOME':str(home/'state'),'PATH':'/usr/local/bin:/usr/bin:/bin','SHELL':'/bin/sh','TERM':'xterm-256color','COLORTERM':'truecolor'}
        os.execve(str(binary),[str(binary),str(path),'--no-mouse'],env)
    os.close(slave); captured=bytearray(); answers={b'\x1b[6n':0,b'\x1b[c':0}; reaped=False
    def pump(seconds):
        deadline=time.monotonic()+seconds
        while time.monotonic()<deadline:
            if not select.select([master],[],[],0.025)[0]: continue
            try: chunk=os.read(master,65536)
            except OSError as error:
                if error.errno==errno.EIO: break
                raise
            if not chunk: break
            captured.extend(chunk)
            assert len(captured)<=1024*1024,'PTY capture exceeded cap'
            for query,answer in [(b'\x1b[6n',b'\x1b[1;1R'),(b'\x1b[c',b'\x1b[?62c')]:
                total=captured.count(query)
                while answers[query]<total: os.write(master,answer); answers[query]+=1
    try:
        deadline=time.monotonic()+5
        while time.monotonic()<deadline:
            pump(0.1)
            if b'\x1b[?1049h' in captured and answers[b'\x1b[6n']>0: break
        else: raise AssertionError('editor did not initialize PTY')
        pump(0.2); os.write(master,b'#'); pump(0.15); os.write(master,b'\x13')
        deadline=time.monotonic()+4
        while time.monotonic()<deadline:
            pump(0.1)
            if wanted(path,bytes(captured)): break
        else: raise AssertionError('save workflow did not reach expected result')
        os.write(master,b'\x11'); pump(0.2)
        if uid: os.write(master,b'd')
        deadline=time.monotonic()+3
        while time.monotonic()<deadline:
            pump(0.1); got,status=os.waitpid(pid,os.WNOHANG)
            if got:
                reaped=True; assert os.waitstatus_to_exitcode(status)==0,('exit',status); break
        assert reaped,'editor failed to quit after reviewed save/discard'
        return bytes(captured)
    finally:
        if not reaped:
            try: os.killpg(pid,signal.SIGKILL)
            except ProcessLookupError: pass
            os.waitpid(pid,0)
        os.close(master)
        print('pty_uid='+str(uid)+' cols='+str(240 if uid else 80)+' capture='+repr(bytes(captured)),flush=True)

acl_dir=base/'acl'; acl_dir.mkdir()
acl_path=acl_dir/'acl.txt'; acl_path.write_text('ACL fixture\n'); os.chmod(acl_path,0o640)
entries=[(1,6,0xffffffff),(2,4,65534),(4,4,0xffffffff),(16,4,0xffffffff),(32,0,0xffffffff)]
acl=struct.pack('<I',2)+b''.join(struct.pack('<HHI',*entry) for entry in entries)
try: os.setxattr(acl_path,'system.posix_acl_access',acl)
except OSError as error:
    print('ACL_BRANCH_UNSUPPORTED errno='+str(error.errno)+' error='+str(error),flush=True)
    assert error.errno in (errno.ENOTSUP,errno.EOPNOTSUPP),'unexpected ACL setup refusal'
else:
    before=os.stat(acl_path); before_acl=os.getxattr(acl_path,'system.posix_acl_access')
    assert before_acl==acl
    output=exercise(acl_path,0,lambda path,out: path.read_text()=='#ACL fixture\n')
    after=os.stat(acl_path); after_acl=os.getxattr(acl_path,'system.posix_acl_access')
    assert (before.st_uid,before.st_gid,before.st_mode)==(after.st_uid,after.st_gid,after.st_mode)
    assert before_acl==after_acl
    assert before.st_ino!=after.st_ino,'expected metadata-preserving atomic replacement'
    print('ACL_PRESERVATION_PASS named_uid=65534 acl_hex='+after_acl.hex()+' mode='+oct(after.st_mode&0o777)+' uid='+str(after.st_uid)+' gid='+str(after.st_gid)+' atomic_inode_replaced=true',flush=True)

cross=base/'cross-owner'; cross.mkdir(); os.chmod(cross,0o777)
path=cross/'root-owned.txt'; path.write_text('root owned fixture\n'); os.chmod(path,0o666)
before=os.stat(path)
assert before.st_uid==0
output=exercise(path,65534,lambda path,out: b'cannot preserve owner/group' in out)
after=os.stat(path)
assert path.read_text()=='root owned fixture\n'
assert (before.st_dev,before.st_ino,before.st_uid,before.st_gid,before.st_mode)==(after.st_dev,after.st_ino,after.st_uid,after.st_gid,after.st_mode)
assert not list(cross.glob('*.tmp'))
assert b'cannot preserve owner/group' in output
print('CROSS_OWNER_REFUSAL_PASS child_uid=65534 original_uid=0 mode=0666 original_bytes_and_inode_unchanged=true no_temporary_leftovers=true',flush=True)
print('packaged_metadata_result=passed',flush=True)
