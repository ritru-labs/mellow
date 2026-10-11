import os,pty,fcntl,termios,struct,time,select,codecs,pathlib,json,hashlib,signal,re
import pyte
BINARY=pathlib.Path(os.environ['MELLOW_UI_TEST_BINARY'])
SOURCE_SHA=os.environ['MELLOW_UI_TEST_SOURCE_SHA']
assert 'source_commit='+SOURCE_SHA in (BINARY.parent/'BUILD.txt').read_text()
BASE=pathlib.Path('/private/tmp/mellow-ui-lsp-'+SOURCE_SHA[:12]+'-final-'+str(time.time_ns()));BASE.mkdir()
print('evidence_root='+str(BASE),flush=True)
TOOLS='/private/tmp/mellow-lsp-tools/node_modules/.bin'
results=[]
class Editor:
 def __init__(self,root,file,env):
  self.root=root;self.reaped=False;self.raw=bytearray();self.screen=pyte.Screen(80,24);self.stream=pyte.Stream(self.screen);self.dec=codecs.getincrementaldecoder('utf8')('replace');self.da=0;self.ds=0;self.shots=[]
  self.pid,self.fd=pty.fork()
  if not self.pid:
   fcntl.ioctl(0,termios.TIOCSWINSZ,struct.pack('HHHH',24,80,0,0));os.chdir(root);os.execve(str(BINARY),[str(BINARY),str(file)],env)
 def pump(self,seconds=.1):
  end=time.monotonic()+seconds
  while time.monotonic()<end:
   ready,_,_=select.select([self.fd],[],[],max(0,min(.03,end-time.monotonic())))
   if not ready:continue
   try:b=os.read(self.fd,65536)
   except OSError:return
   if not b:return
   self.raw.extend(b);self.stream.feed(self.dec.decode(b))
   raw=bytes(self.raw)
   q=raw.count(b'\x1b[6n');a=raw.count(b'\x1b[c')
   while self.ds<q:os.write(self.fd,b'\x1b[1;1R');self.ds+=1
   while self.da<a:os.write(self.fd,b'\x1b[?62c');self.da+=1
 def text(self):return '\n'.join(self.screen.display)
 def send(self,b):os.write(self.fd,b);self.pump(.12)
 def wait(self,name,predicate,timeout=12):
  end=time.monotonic()+timeout
  while time.monotonic()<end:
   self.pump(.1)
   if predicate(self.text()):self.shots.append((name,self.text()));return
  self.shots.append(('FAILED '+name,self.text()));raise AssertionError(name+' not observed\n'+self.text())
 def command(self,name):
  self.send(b'\x10');self.wait('palette',lambda s:'Commands' in s)
  self.send(name.encode());self.send(b'\r')
 def close(self):
  (self.root/'terminal.raw').write_bytes(self.raw)
  (self.root/'screens.txt').write_text('\n\n'.join('=== '+n+' ===\n'+s for n,s in self.shots))
  if not self.reaped:
   try:
    pid,status=os.waitpid(self.pid,os.WNOHANG)
    if pid==self.pid:self.reaped=True
   except ChildProcessError:self.reaped=True
  if not self.reaped:
   # The unreaped direct child still owns its PID. Never signal a reaped PID.
   try:
    if os.getpgid(self.pid)==self.pid:os.killpg(self.pid,signal.SIGKILL)
    else:os.kill(self.pid,signal.SIGKILL)
   except ProcessLookupError:pass
   deadline=time.monotonic()+2
   while time.monotonic()<deadline:
    try:pid,status=os.waitpid(self.pid,os.WNOHANG)
    except ChildProcessError:self.reaped=True;break
    if pid==self.pid:self.reaped=True;break
    time.sleep(.02)
  os.close(self.fd)
for language,extension in [('python','py'),('typescript','ts')]:
 root=BASE/(language+'-workspace');root.mkdir(exist_ok=True)
 config=root/'config/mellow';config.mkdir(parents=True,exist_ok=True)
 (config/'settings.conf').write_text('auto_completion = false\nformat_on_save = false\n')
 main=root/('main.'+extension);main.write_text('audit_sy')
 helper=root/('helper.'+extension)
 helper.write_text('def audit_symbol() -> int:\n    return 42\n' if language=='python' else 'export function audit_symbol(): number { return 42; }\n')
 (root/'pyrightconfig.json').write_text('{"include":["*.py"],"typeCheckingMode":"basic"}')
 (root/'tsconfig.json').write_text('{"compilerOptions":{"strict":true,"module":"commonjs","target":"ES2020"},"include":["*.ts"]}')
 env=os.environ.copy();env.update(TERM='xterm-256color',XDG_CONFIG_HOME=str(root/'config'),XDG_STATE_HOME=str(root/'state'),MELLOW_LSP_PYTHON=TOOLS+'/pyright-langserver',MELLOW_LSP_TYPESCRIPT=TOOLS+'/typescript-language-server',PATH=TOOLS+':'+env['PATH'])
 e=Editor(root,main,env);result={'language':language,'checks':[],'source_commit':SOURCE_SHA,'binary_sha256':hashlib.sha256(BINARY.read_bytes()).hexdigest()}
 try:
  e.wait('startup',lambda s:'Ln ' in s or 'Welcome to Mellow' in s)
  if 'Welcome to Mellow' in e.text():e.send(b'\x1b')
  e.wait('editing',lambda s:'Ln ' in s and 'Welcome to Mellow' not in s)
  e.command('Trust workspace execution');e.wait('trust review',lambda s:'Press Enter to explicitly trust' in s);e.send(b'\r')
  e.wait('trusted',lambda s:'Workspace trusted' in s or 'Pyright ready' in s or 'TypeScript language server' in s)
  e.pump(2)
  e.send(b'\x0f');e.wait('helper picker',lambda s:'Open file' in s);e.send(str(helper).encode());e.pump(.5);e.send(b'\r');e.wait('helper opened',lambda s:('helper.'+extension) in s and 'return 42' in s);e.pump(.6)
  e.send(b'\x1b[6;5~');e.wait('main selected',lambda s:s.splitlines()[1].strip().startswith('main.'+extension) and 'audit_sy' in s);e.send(b'\x1b[F')
  e.command('Trigger completion');e.wait('actual autoimport popup',lambda s:'audit_symbol' in s,20)
  e.send(b'\r');e.wait('completion applied to editor',lambda s:'Suggestions' not in s and 'audit_symbol' in s and 'helper' in s,15);e.send(b'\x13');e.pump(.4)
  text=main.read_text();assert 'audit_symbol' in text and 'helper' in text,repr(text)
  result['checks'].append('packaged keyboard completion adds helper import and symbol');result['completed_text']=text
  e.send(b'\x1a');e.send(b'\x13');e.pump(.3);assert main.read_text()=='audit_sy',repr(main.read_text())
  result['checks'].append('one undo restores both primary edit and import')
  e.send(b'\x19');e.send(b'\x13');e.pump(.3);assert main.read_text()==text
  result['checks'].append('redo restores both edits')
  e.send(b'\x1b[D');e.command('Go to definition');e.wait('helper definition opened',lambda s:s.splitlines()[1].strip().startswith('helper.'+extension) and 'return 42' in s,15)
  result['checks'].append('keyboard definition opens actual helper file')
  e.command('Navigation back');e.wait('return main',lambda s:s.splitlines()[1].strip().startswith('main.'+extension) and 'audit_symbol' in s)
  # Bracketed paste changes the actual buffer; server diagnostics must appear in
  # the packaged Problems panel rather than being inferred from server output.
  e.send(b'\x1b[1;5F');e.send(b'\x1b[200~\naudit_missing_name\n\x1b[201~');e.pump(2)
  e.command('Show problems');e.wait('undefined diagnostic in Problems',lambda s:'Problems' in s and any(re.search(r'\b[45]:1\s+main\.'+extension, row) and 'audit_missing' in row for row in s.splitlines()),20)
  result['checks'].append('actual Problems panel shows undefined-name diagnostic')
  e.send(b'\x1b');e.send(b'\x13');e.send(b'\x11');e.pump(.3)
  deadline=time.monotonic()+5;pid=0;status=None
  while time.monotonic()<deadline:
   pid,status=os.waitpid(e.pid,os.WNOHANG)
   if pid==e.pid:e.reaped=True;break
   e.pump(.05)
  assert pid==e.pid and os.waitstatus_to_exitcode(status)==0,(pid,status)
  result['editor_pid']=e.pid;result['clean_exit_code']=os.waitstatus_to_exitcode(status)
  result['checks'].append('save and clean keyboard exit');result['status']='passed'
 except Exception as ex:e.shots.append(('failure final screen',e.text()));result['status']='failed';result['failure']=str(ex)
 finally:
  results.append(result);(BASE/'results.json').write_text(json.dumps(results,indent=2));print(json.dumps(result),flush=True)
  e.close()
assert all(r['status']=='passed' for r in results),results
