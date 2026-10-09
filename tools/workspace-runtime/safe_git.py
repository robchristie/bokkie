"""Commit-derived source observations without candidate Git configuration/code."""
import ctypes
import errno
import hashlib
from functools import lru_cache
import json
import os
from pathlib import Path
import re
import select
import selectors
import signal
import stat
import subprocess
import sys
import tempfile
import time

MAX_METADATA=8*1024*1024
MAX_OBJECT=8*1024*1024
MAX_FILES=32768
MAX_FILE_BYTES=64*1024*1024
MAX_TOTAL_BYTES=512*1024*1024
MAX_SECONDS=20
MAX_DIAGNOSTIC=4096
GIT='/usr/bin/git'


@lru_cache(maxsize=8)
def require_fd_binding(bwrap,identity):
    value=subprocess.run([bwrap,'--help'],env={'PATH':'/usr/bin:/bin','LANG':'C','LC_ALL':'C'},
        stdout=subprocess.PIPE,stderr=subprocess.DEVNULL,timeout=2)
    if value.returncode or len(value.stdout)>65536 or b'--ro-bind-fd ' not in value.stdout:
        raise ValueError('safe Git requires Bubblewrap FD-consuming read-only bind capability')


def directory(path):
    path=Path(path)
    if not path.is_absolute():raise ValueError('source path is not absolute')
    descriptor=os.open('/',os.O_RDONLY|os.O_DIRECTORY)
    try:
        for part in path.parts[1:]:
            child=os.open(part,os.O_RDONLY|os.O_DIRECTORY|os.O_NOFOLLOW,dir_fd=descriptor)
            os.close(descriptor);descriptor=child
        return descriptor
    except Exception:
        os.close(descriptor);raise


def fingerprint(value):
    return (value.st_dev,value.st_ino,value.st_mode,value.st_size,value.st_mtime_ns,value.st_ctime_ns)


def regular(path,limit=MAX_METADATA):
    path=Path(path);parent=directory(path.parent)
    try:
        descriptor=os.open(path.name,os.O_RDONLY|os.O_NOFOLLOW|os.O_NONBLOCK,dir_fd=parent)
        try:
            before=os.fstat(descriptor)
            if not stat.S_ISREG(before.st_mode) or before.st_nlink!=1 or before.st_size>limit:
                raise ValueError('source metadata is not a bounded single-link regular file')
            chunks=[];remaining=limit+1
            while remaining:
                value=os.read(descriptor,min(65536,remaining))
                if not value:break
                chunks.append(value);remaining-=len(value)
            raw=b''.join(chunks)
            if (len(raw)>limit or fingerprint(before)!=fingerprint(os.fstat(descriptor)) or
                    fingerprint(before)!=fingerprint(os.stat(path.name,dir_fd=parent,follow_symlinks=False))):
                raise ValueError('source metadata changed during observation')
            return raw
        finally:os.close(descriptor)
    finally:os.close(parent)


def exists(path):
    try:os.lstat(path);return True
    except FileNotFoundError:return False


def canonical_data_path(value):
    path=Path(os.path.abspath(value))
    descriptor=directory(path)
    try:
        if path.resolve(strict=True)!=path:raise ValueError('source directory is aliased')
    finally:os.close(descriptor)
    return path


def discover(cwd,allowed_common,registered_checkout=None):
    cwd=canonical_data_path(cwd)
    worktree=next((p for p in (cwd,*cwd.parents) if exists(p/'.git')),None)
    if worktree is None:raise ValueError('source has no supported Git entry')
    marker=worktree/'.git';mode=os.lstat(marker).st_mode
    if stat.S_ISDIR(mode):gitdir=canonical_data_path(marker)
    elif stat.S_ISREG(mode):
        value=regular(marker,4096).decode('utf-8').strip()
        if not value.startswith('gitdir: '):raise ValueError('unsupported Git entry')
        target=Path(value[8:]);gitdir=canonical_data_path(target if target.is_absolute() else worktree/target)
    else:raise ValueError('Git entry is aliased or unsupported')
    common=gitdir
    if exists(gitdir/'commondir'):
        value=regular(gitdir/'commondir',4096).decode('utf-8').strip()
        if not value:raise ValueError('empty Git common directory')
        common=canonical_data_path(gitdir/value)
    if str(common) not in allowed_common:raise ValueError('Git common directory is outside admission')
    if common!=gitdir:
        if gitdir.parent!=common/'worktrees':raise ValueError('unregistered linked worktree directory')
        back=regular(gitdir/'gitdir',4096).decode('utf-8').strip()
        if Path(os.path.abspath(back))!=marker:raise ValueError('linked worktree backlink mismatch')
    elif registered_checkout is not None and worktree!=Path(registered_checkout):
        raise ValueError('source is not the admitted main checkout')
    if exists(common/'reftable') or exists(gitdir/'reftable'):
        raise ValueError('reftable source layout is unsupported')
    objects=canonical_data_path(common/'objects')
    if exists(objects/'info/alternates') and regular(objects/'info/alternates',4096).strip():
        raise ValueError('alternate object stores are unsupported')
    return {'worktree':str(worktree),'gitdir':str(gitdir),'common':str(common),'objects':str(objects)}


def resolve_head(layout):
    value=regular(Path(layout['gitdir'])/'HEAD',4096).strip()
    visited=set()
    for _ in range(8):
        if re.fullmatch(rb'[0-9a-f]{40}',value):return value.decode()
        if not value.startswith(b'ref: refs/'):
            raise ValueError('source HEAD is not a SHA-1 commit reference')
        name=value[5:].decode('utf-8')
        if name in visited or any(p in ('','.', '..') for p in name.split('/')) or any(c in name for c in '\\:\x00\n\r'):
            raise ValueError('invalid or cyclic source reference')
        visited.add(name)
        loose=Path(layout['common'])/name
        if exists(loose):value=regular(loose,4096).strip();continue
        packed=Path(layout['common'])/'packed-refs'
        if not exists(packed):raise ValueError('source reference is unavailable')
        matches=[]
        for line in regular(packed).splitlines():
            if not line or line.startswith((b'#',b'^')):continue
            parts=line.split(b' ',1)
            if len(parts)!=2 or not re.fullmatch(rb'[0-9a-f]{40}',parts[0]):
                raise ValueError('invalid packed reference data')
            if parts[1].decode('utf-8')==name:matches.append(parts[0])
        if len(matches)!=1:raise ValueError('source reference is ambiguous or unavailable')
        value=matches[0]
    raise ValueError('source reference depth exceeds bound')


_SUPERVISOR=r'''
import ctypes,json,os,select,signal,subprocess,sys,time
stopping=False
def stop(*args):
 global stopping
 stopping=True
signal.signal(signal.SIGTERM,stop);signal.signal(signal.SIGINT,stop)
if ctypes.CDLL(None,use_errno=True).prctl(36,1,0,0,0)!=0:sys.exit(125)
parent=int(sys.argv[1]);fds=json.loads(sys.argv[2]);command=json.loads(sys.argv[3]);child=None
try:
 if not select.select([parent],[],[],0)[0]:
  child=subprocess.Popen(command,pass_fds=tuple(fds))
  while not stopping and not select.select([parent],[],[],.05)[0]:
   if child.poll() is not None:break
finally:
 children='/proc/self/task/'+str(os.getpid())+'/children'
 while True:
  try:
   while True:
    pid,status=os.waitpid(-1,os.WNOHANG)
    if pid==0:break
  except ChildProcessError:break
  for pid in open(children).read().split():
   try:os.kill(int(pid),signal.SIGKILL)
   except ProcessLookupError:pass
  time.sleep(.01)
os.close(parent)
'''


def pidfd(pid):
    # CPython's native kernel API works on glibc releases without the exported
    # wrapper. Preserve any native error as an unavailable monitoring boundary.
    native=getattr(os,'pidfd_open',None)
    if native is not None:return native(pid,0)
    libc=ctypes.CDLL(None,use_errno=True)
    operation=getattr(libc,'pidfd_open',None)
    if operation is None:
        raise OSError(errno.ENOSYS,'safe Git parent pidfd unavailable: no native or libc pidfd_open')
    operation.argtypes=[ctypes.c_int,ctypes.c_uint];operation.restype=ctypes.c_int
    descriptor=operation(pid,0)
    if descriptor<0:raise OSError(ctypes.get_errno(),'safe Git parent pidfd unavailable')
    return descriptor


class Pipe:
    def __init__(self,command,environment,fds,deadline):
        self.deadline=deadline;self.buffer=b'';self.diagnostic=b'';parent=pidfd(os.getpid())
        try:
            self.child=subprocess.Popen([sys.executable,'-I','-c',_SUPERVISOR,str(parent),json.dumps(fds),json.dumps(command)],
                env=environment,pass_fds=(parent,*fds),stdin=subprocess.PIPE,stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,start_new_session=True)
        finally:os.close(parent)
        os.set_blocking(self.child.stdin.fileno(),False);os.set_blocking(self.child.stdout.fileno(),False)
        os.set_blocking(self.child.stderr.fileno(),False)

    def drain_diagnostic(self):
        while True:
            try:value=os.read(self.child.stderr.fileno(),MAX_DIAGNOSTIC+1)
            except BlockingIOError:break
            if not value:break
            self.diagnostic+=value
            if len(self.diagnostic)>MAX_DIAGNOSTIC:
                self.diagnostic=self.diagnostic[:MAX_DIAGNOSTIC]
                raise self.failure('safe Git helper diagnostic exceeds bound')

    def failure(self,message):
        # The helper has a fresh environment and only trusted executable/library
        # mounts plus private Git data. Retain bounded diagnostics from that
        # boundary; never dump inherited host environment or candidate config.
        detail=self.diagnostic.decode('utf-8',errors='replace').strip()
        return ValueError(message+(' (helper stderr: '+json.dumps(detail)+')' if detail else ''))

    def send(self,value):
        remaining=memoryview(value)
        while remaining:
            self.check()
            try:remaining=remaining[os.write(self.child.stdin.fileno(),remaining):]
            except BlockingIOError:select.select([],[self.child.stdin.fileno()],[],.05)
            except BrokenPipeError:
                self.drain_diagnostic();raise self.failure('safe Git observation transport ended')

    def check(self):
        if time.monotonic()>=self.deadline:raise TimeoutError('safe Git observation deadline exhausted')
        self.drain_diagnostic()

    def take(self,count=None,delimiter=None):
        while True:
            self.check()
            if delimiter is not None and delimiter in self.buffer:
                result,self.buffer=self.buffer.split(delimiter,1);return result
            if count is not None and len(self.buffer)>=count:
                result,self.buffer=self.buffer[:count],self.buffer[count:];return result
            if len(self.buffer)>MAX_OBJECT+4096:raise ValueError('safe Git output exceeds bound')
            ready=select.select([self.child.stdout,self.child.stderr],[],[],.05)[0]
            if self.child.stderr in ready:self.drain_diagnostic()
            if self.child.stdout not in ready:continue
            value=os.read(self.child.stdout.fileno(),65536)
            if not value:
                self.drain_diagnostic();raise self.failure('safe Git observation transport ended')
            self.buffer+=value

    def close(self):
        if self.child.poll() is None:self.child.terminate()
        self.child.wait(timeout=5)
        self.child.stdin.close();self.child.stdout.close();self.child.stderr.close()


class Objects:
    def __init__(self,layout,private_root,bwrap='/usr/bin/bwrap'):
        require_fd_binding(bwrap,fingerprint(os.stat(bwrap)))
        self.deadline=time.monotonic()+MAX_SECONDS
        self.temp=tempfile.TemporaryDirectory(prefix='safe-git-',dir=private_root)
        self.root=Path(self.temp.name);self.git=self.root/'git';self.git.mkdir()
        for name in ('objects','refs','info'):(self.git/name).mkdir()
        (self.git/'HEAD').write_text('ref: refs/heads/unused\n')
        (self.git/'config').write_text('[core]\nrepositoryformatversion=0\nbare=false\nfsmonitor=false\nhooksPath=/dev/null\nattributesFile=/dev/null\nexcludesFile=/dev/null\n')
        self.work=self.root/'work';self.work.mkdir()
        self.object_fd=None;self.git_fd=None;self.work_fd=None
        self.bwrap=bwrap;self.pipes=[]
        self.environment={'PATH':'/usr/bin:/bin','LANG':'C','LC_ALL':'C','PYTHONNOUSERSITE':'1',
            'GIT_CONFIG_NOSYSTEM':'1','GIT_CONFIG_SYSTEM':'/dev/null','GIT_CONFIG_GLOBAL':'/dev/null',
            'GIT_ATTR_NOSYSTEM':'1','GIT_NO_REPLACE_OBJECTS':'1','GIT_TERMINAL_PROMPT':'0',
            'GIT_OPTIONAL_LOCKS':'0','GIT_FLUSH':'1','GIT_OBJECT_DIRECTORY':'/objects'}
        try:
            self.object_fd=directory(layout['objects']);self.git_fd=directory(self.git);self.work_fd=directory(self.work)
            self.batch=self.pipe(['cat-file','--batch'])
        except Exception:
            self.close();raise
        self.cache={}
        self.object_bytes=0;self.tree_entries=0

    def pipe(self,args):
        command=[self.bwrap,'--unshare-all','--die-with-parent','--new-session','--cap-drop','ALL','--proc','/proc','--dev','/dev',
                 '--tmpfs','/tmp','--ro-bind',GIT,GIT]
        # Only trusted executable/library trees; no host root, /home or /etc.
        for path in ('/usr/lib','/usr/lib64','/lib','/lib64'):
            if Path(path).exists():command+=['--ro-bind',str(Path(path).resolve()),path]
        # FD-consuming mounts avoid retaining original-host directory aliases
        # in the payload after construction. Plain /proc/self/fd binds leak them.
        command+=['--ro-bind-fd',str(self.object_fd),'/objects',
                  '--ro-bind-fd',str(self.git_fd),'/git',
                  '--ro-bind-fd',str(self.work_fd),'/work','--chdir','/work',
                  '--',GIT,'--no-lazy-fetch','--no-optional-locks','--git-dir=/git','--work-tree=/work',*args]
        pipe=Pipe(command,self.environment,[self.object_fd,self.git_fd,self.work_fd],self.deadline)
        self.pipes.append(pipe);return pipe

    def object(self,oid,expected):
        self.batch.check()
        if oid in self.cache:
            kind,raw=self.cache[oid]
            if kind!=expected:raise ValueError('cached Git object type differs')
            return raw
        self.batch.send(oid.encode()+b'\n');header=self.batch.take(delimiter=b'\n').split()
        if len(header)!=3 or header[0]!=oid.encode() or header[1].decode()!=expected:
            raise ValueError('Git object is missing or has the wrong type')
        size=int(header[2])
        if not 0<=size<=MAX_OBJECT:raise ValueError('Git object exceeds observation bound')
        self.object_bytes+=size
        if self.object_bytes>MAX_TOTAL_BYTES:raise ValueError('Git object aggregate exceeds observation bound')
        raw=self.batch.take(count=size)
        if self.batch.take(count=1)!=b'\n' or hashlib.sha1(header[1]+b' '+str(size).encode()+b'\0'+raw).hexdigest()!=oid:
            raise ValueError('Git object digest does not match its identity')
        self.cache[oid]=(expected,raw);return raw

    def close(self):
        failure=None
        try:
            for pipe in self.pipes:
                try:pipe.close()
                except Exception as error:failure=error
        finally:
            for descriptor in (self.object_fd,self.git_fd,self.work_fd):
                if descriptor is not None:os.close(descriptor)
            self.temp.cleanup()
        if failure is not None:raise failure


def tree_files(objects,tree,prefix=(),result=None):
    result={} if result is None else result
    if len(prefix)>64:raise ValueError('Git tree depth exceeds bound')
    raw=objects.object(tree,'tree');offset=0
    while offset<len(raw):
        objects.batch.check();objects.tree_entries+=1
        if objects.tree_entries>MAX_FILES:raise ValueError('Git tree entry aggregate exceeds bound')
        space=raw.index(b' ',offset);end=raw.index(b'\0',space)
        mode=raw[offset:space];name=raw[space+1:end];oid=raw[end+1:end+21].hex();offset=end+21
        if not name or name in (b'.',b'..') or name.lower()==b'.git' or b'/' in name or len(oid)!=40:
            raise ValueError('unsafe Git tree path')
        path=prefix+(os.fsdecode(name),)
        if mode==b'40000':tree_files(objects,oid,path,result)
        elif mode in (b'100644',b'100755',b'120000'):
            if path in result or len(result)>=MAX_FILES:raise ValueError('Git tree file catalogue invalid or oversized')
            result[path]=(mode.decode(),oid)
        else:raise ValueError('submodule or unsupported Git tree mode')
    if offset!=len(raw):raise ValueError('truncated Git tree')
    return result


def commit_tree(objects,head):
    raw=objects.object(head,'commit');line=raw.split(b'\n',1)[0]
    if not re.fullmatch(rb'tree [0-9a-f]{40}',line):raise ValueError('commit tree identity is invalid')
    return line[5:].decode()


class Ignore:
    def __init__(self,objects,files):
        for path,(mode,oid) in files.items():
            if path[-1]!='.gitignore':continue
            if mode not in ('100644','100755'):raise ValueError('committed ignore file is not regular')
            raw=objects.object(oid,'blob')
            if len(raw)>65536:raise ValueError('committed ignore policy exceeds bound')
            target=objects.work.joinpath(*path);target.parent.mkdir(parents=True,exist_ok=True);target.write_bytes(raw)
        self.pipe=objects.pipe(['check-ignore','--no-index','--stdin','-z','--verbose','--non-matching'])

    def ignored(self,path,is_directory):
        name=os.fsencode('/'.join(path))+(b'/' if is_directory else b'')
        self.pipe.send(name+b'\0')
        source,line,pattern,returned=(self.pipe.take(delimiter=b'\0') for _ in range(4))
        if returned!=name:raise ValueError('ignore response path mismatch')
        return bool(pattern and not pattern.startswith(b'!'))


def compare_worktree(objects,worktree,files):
    ignore=Ignore(objects,files);seen=set();total=0;visited=0;clean=True
    tracked_dirs={path[:i] for path in files for i in range(1,len(path))}
    root=directory(worktree)
    def walk(descriptor,prefix):
        nonlocal total,visited,clean
        before=os.fstat(descriptor)
        for name in os.listdir(descriptor):
            objects.batch.check();path=prefix+(name,)
            if not prefix and name=='.git':continue
            visited+=1
            if visited>MAX_FILES:raise ValueError('filesystem entry catalogue exceeds bound')
            value=os.stat(name,dir_fd=descriptor,follow_symlinks=False)
            if stat.S_ISDIR(value.st_mode):
                if path not in tracked_dirs and ignore.ignored(path,True):continue
                child=os.open(name,os.O_RDONLY|os.O_DIRECTORY|os.O_NOFOLLOW,dir_fd=descriptor)
                try:walk(child,path)
                finally:os.close(child)
            elif path not in files:
                if not ignore.ignored(path,False):clean=False
            else:
                seen.add(path);mode,oid=files[path]
                if mode=='120000':
                    if not stat.S_ISLNK(value.st_mode):clean=False;continue
                    raw=os.fsencode(os.readlink(name,dir_fd=descriptor))
                    actual=hashlib.sha1(b'blob '+str(len(raw)).encode()+b'\0'+raw).hexdigest()
                elif stat.S_ISREG(value.st_mode) and value.st_nlink==1:
                    if value.st_size>MAX_FILE_BYTES:raise ValueError('tracked file exceeds observation bound')
                    total+=value.st_size
                    if total+objects.object_bytes>MAX_TOTAL_BYTES:raise ValueError('source byte aggregate exceeds bound')
                    fd=os.open(name,os.O_RDONLY|os.O_NOFOLLOW|os.O_NONBLOCK,dir_fd=descriptor)
                    try:
                        opened=os.fstat(fd)
                        if fingerprint(opened)!=fingerprint(value):raise ValueError('source path replaced during observation')
                        hasher=hashlib.sha1(b'blob '+str(opened.st_size).encode()+b'\0');read_bytes=0
                        while True:
                            objects.batch.check();raw=os.read(fd,65536)
                            if not raw:break
                            read_bytes+=len(raw)
                            if read_bytes>opened.st_size:raise ValueError('source file grew during observation')
                            hasher.update(raw)
                        if read_bytes!=opened.st_size or fingerprint(opened)!=fingerprint(os.fstat(fd)):
                            raise ValueError('source file changed during observation')
                        actual=hasher.hexdigest()
                    finally:os.close(fd)
                    if bool(value.st_mode&stat.S_IXUSR)!=(mode=='100755'):clean=False
                else:clean=False;continue
                if fingerprint(value)!=fingerprint(os.stat(name,dir_fd=descriptor,follow_symlinks=False)):
                    raise ValueError('source path changed during observation')
                if actual!=oid:clean=False
        if fingerprint(before)!=fingerprint(os.fstat(descriptor)):
            raise ValueError('source directory changed during observation')
    try:
        walk(root,())
        if fingerprint(os.fstat(root))!=fingerprint(os.stat(worktree,follow_symlinks=False)):
            raise ValueError('source root changed during observation')
    finally:os.close(root)
    return clean and seen==set(files)


def observe(profile,cwd,private_root,bwrap='/usr/bin/bwrap',*,full=False):
    private_root=canonical_data_path(private_root)
    if any(private_root.is_relative_to(Path(p)) or Path(p).is_relative_to(private_root) for p in profile['write_roots']):
        raise ValueError('observation storage overlaps candidate writable paths')
    if not any(Path(cwd).is_relative_to(Path(p)) for p in profile['write_roots']):
        raise ValueError('command cwd is outside declared product roots')
    layout=discover(cwd,profile['git_common_dirs'])
    repository=next(r for r in profile['verification']['repositories'] if r['git_common_dir']==layout['common'])
    layout=discover(cwd,profile['git_common_dirs'],repository['checkout'])
    head=resolve_head(layout);objects=None
    try:
        objects=Objects(layout,private_root,bwrap);tree=commit_tree(objects,head)
        clean=compare_worktree(objects,layout['worktree'],tree_files(objects,tree)) if full else None
        if discover(cwd,profile['git_common_dirs'],repository['checkout'])!=layout or resolve_head(layout)!=head:
            raise ValueError('source identity changed during observation')
        return {'available':True,'cwd':cwd,'repository':repository['repository'],'head':head,'tree':tree,
                'clean':clean,'capture':'verified_raw_tree' if full else 'verified_metadata','method':'safe-git-v1'}
    finally:
        if objects is not None:objects.close()
