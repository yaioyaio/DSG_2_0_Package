"""DSG 2.0 safety-first working POC. Pure stdlib; no external dependencies."""
import os, json, sqlite3, hashlib, subprocess, shutil, time, re, shlex
import stat
from pathlib import Path
from datetime import datetime, timezone

DEFAULT = {
 'version':2,'project_id':'local-project',
 'project_budget_gb':30,'host_min_free_gb':20,
 'warning_percent':75,'critical_percent':90,
 'scan_dirs':['.next','dist','build','target','coverage','test-results','playwright-report','.pytest_cache','.turbo'],
 'protected_names':['.git','node_modules','.venv','.env','data','uploads','postgres-data','mysql-data'],
 'gc_min_age_hours':168,'max_candidates':100,'max_scan_files':200000
}

def utc(): return datetime.now(timezone.utc).isoformat()
def state_dir():
 p=Path(os.environ.get('DSG_STATE_HOME',str(Path.home()/'.dsg-poc'))).expanduser().resolve();p.mkdir(parents=True,exist_ok=True);return p

def db():
 c=sqlite3.connect(state_dir()/'state.sqlite3',timeout=15);c.execute('PRAGMA journal_mode=WAL');c.execute('CREATE TABLE IF NOT EXISTS audit (id INTEGER PRIMARY KEY, ts TEXT, action TEXT, project TEXT, details TEXT)');return c

def audit(action, project, details):
 with db() as c:c.execute('INSERT INTO audit(ts,action,project,details) VALUES (?,?,?,?)',(utc(),action,str(project),json.dumps(details,ensure_ascii=False)))

def config(root):
 cfg=DEFAULT.copy(); f=Path(root)/'.dsg.json'
 if f.exists():
  raw=json.loads(f.read_text(encoding='utf-8'))
  if not isinstance(raw,dict):raise ValueError('Config must be a JSON object')
  allowed=set(DEFAULT)
  unknown=set(raw)-allowed
  if unknown:raise ValueError('Unknown configuration keys: '+', '.join(sorted(unknown)))
  cfg.update(raw)
 for key in ('protected_names', 'scan_dirs'):
  if not isinstance(cfg[key], list) or not all(isinstance(name, str) for name in cfg[key]):
   raise ValueError('Invalid '+key)
 cfg['protected_names'] = list(dict.fromkeys(DEFAULT['protected_names'] + cfg['protected_names']))
 if type(cfg['max_scan_files']) is not int or cfg['max_scan_files'] < 0:
  raise ValueError('Invalid max_scan_files')
 for key in ('project_budget_gb','host_min_free_gb'):
  if not isinstance(cfg[key],(int,float)) or cfg[key]<0:raise ValueError('Invalid '+key)
 if not 0<cfg['warning_percent']<cfg['critical_percent']<=100:raise ValueError('Invalid thresholds')
 return cfg

def size_tree(path, limit=200000):
 total=0;count=0;errors=[]; stack=[Path(path)]
 while stack:
  p=stack.pop()
  try:
   if p.is_symlink():continue
   if p.is_dir():
    with os.scandir(p) as it:
     for e in it:
      if e.is_symlink():continue
      stack.append(Path(e.path))
   else:
    total+=p.stat(follow_symlinks=False).st_size;count+=1
    if count>=limit:return {'bytes':total,'files':count,'truncated':True,'errors':errors}
  except (OSError,PermissionError) as e:errors.append(str(e))
 return {'bytes':total,'files':count,'truncated':False,'errors':errors}

def scan(root):
 root=Path(root).resolve(strict=True);cfg=config(root);entries=[]
 for rel in cfg['scan_dirs']:
  if not _direct_child_name(rel):continue
  p=root/rel
  if p.is_symlink() or not p.exists() or not p.is_dir():continue
  if p.resolve().parent!=root:continue  # only top-level known outputs
  d=size_tree(p,cfg['max_scan_files']);entries.append({'path':rel,**d,'mtime':p.stat().st_mtime})
 usage=shutil.disk_usage(root)
 return {'project_id':cfg['project_id'],'root':str(root),'entries':entries,'total_bytes':sum(x['bytes'] for x in entries),'disk_free_bytes':usage.free,'disk_total_bytes':usage.total,'policy':{'project_budget_gb':cfg['project_budget_gb'],'host_min_free_gb':cfg['host_min_free_gb']},'timestamp':utc()}

def deny_command(s):
 """Advisory only. It cannot parse arbitrary shell languages or prevent API bypass."""
 x=' '.join(s.strip().lower().split())
 bad=[('docker_volume_delete',r'\bdocker\s+(?:volume\s+(?:prune|rm)|system\s+prune\b[^\n]*(?:--volumes|-a))'),('compose_volume_delete',r'\bdocker\s+compose\s+down\b[^\n]*(?:\s-v\b|--volumes)'),('recursive_delete',r'\b(?:rm\s+-[^\n]*r|rmdir\s+/s|remove-item\b[^\n]*-recurse)')]
 for name,p in bad:
  if re.search(p,x):return {'decision':'deny','reason':name,'advisory':True}
 if re.search(r'\b(?:docker\s+(?:build|run|create|compose\s+up)|npm\s+run\s+build|pnpm\s+(?:build|test)|cargo\s+(?:build|test)|gradle(?:w)?\s+.*build|playwright\s+test)\b',x):
  return {'decision':'warn','reason':'managed_execution_recommended','suggest':'Run through dsg exec -- ...','advisory':True}
 return {'decision':'allow','reason':'no_known_risk','advisory':True}

def _direct_child_name(name):
    return (isinstance(name, str) and bool(name) and Path(name).name == name
            and name not in ('.', '..') and not any(c in name for c in ('/', '\\', ':', '\x00'))
            and not name.endswith((' ', '.')))


def _protected(name, protected_names):
    name = name.casefold()
    return (name in protected_names or name.endswith('.env') or '.env.' in name
            or ('.deploy.' in name and name.endswith('env')))


def _check_boundary(path, metadata, device, protected_names):
    if _protected(path.name, protected_names):
        raise ValueError('Protected target')
    if (stat.S_ISLNK(metadata.st_mode)
            or getattr(metadata, 'st_file_attributes', 0) & stat.FILE_ATTRIBUTE_REPARSE_POINT
            or metadata.st_dev != device or os.path.ismount(path)):
        raise ValueError('Unsafe target boundary')


def _inspect_candidate(root, rel, cfg, protected_names, saved=None):
    if not _direct_child_name(rel) or rel not in cfg['scan_dirs']:
        raise ValueError('Unmanaged target')
    target = root / rel
    metadata = target.lstat()
    device = root.stat().st_dev
    _check_boundary(target, metadata, device, protected_names)
    if not stat.S_ISDIR(metadata.st_mode):
        raise ValueError('Unsafe target')
    resolved = target.resolve(strict=True)
    if resolved.parent != root or _protected(resolved.name, protected_names):
        raise ValueError('Unmanaged or protected target')
    identity = (metadata.st_ino, metadata.st_dev, metadata.st_mtime_ns)
    if saved is not None and identity != (saved['inode'], saved['device'], saved['mtime_ns']):
        raise ValueError('Resource modified since plan: ' + rel)
    age = (time.time() - metadata.st_mtime) / 3600
    if age < cfg['gc_min_age_hours']:
        raise ValueError('Resource no longer expired')

    total = 0
    visited = 0
    pending = [target]
    while pending:
        with os.scandir(pending.pop()) as entries:
            for entry in entries:
                visited += 1
                if visited > cfg['max_scan_files']:
                    raise ValueError('Target exceeds scan limit')
                child = Path(entry.path)
                info = entry.stat(follow_symlinks=False)
                _check_boundary(child, info, device, protected_names)
                if stat.S_ISDIR(info.st_mode):
                    pending.append(child)
                elif stat.S_ISREG(info.st_mode):
                    total += info.st_size
                else:
                    raise ValueError('Unsupported target file type')
    return {'relative_path': rel, 'bytes': total, 'age_hours': round(age, 1),
            'inode': metadata.st_ino, 'device': metadata.st_dev, 'mtime_ns': metadata.st_mtime_ns,
            'reason': 'expired_generated_output'}


def plan(root):
    root = Path(root).resolve(strict=True)
    cfg = config(root)
    protected = {name.casefold() for name in cfg['protected_names']}
    candidates = []
    seen = set()
    for rel in cfg['scan_dirs']:
        if len(candidates) >= cfg['max_candidates']:
            break
        try:
            candidate = _inspect_candidate(root, rel, cfg, protected)
        except (ValueError, OSError, RuntimeError):
            continue
        key = (candidate['device'], candidate['inode'])
        if key not in seen:
            candidates.append(candidate)
            seen.add(key)

    payload = {'version': 1, 'project_id': cfg['project_id'], 'root': str(root),
               'created_at': utc(), 'candidates': candidates, 'dry_run': True}
    raw = json.dumps(payload, sort_keys=True, ensure_ascii=False).encode()
    payload['plan_id'] = hashlib.sha256(raw).hexdigest()[:16]
    target = state_dir() / ('plan-' + payload['plan_id'] + '.json')
    target.write_text(json.dumps(payload, ensure_ascii=False, indent=2), encoding='utf8')
    audit('gc_plan', root, {'id': payload['plan_id'], 'count': len(candidates)})
    return payload


def apply_plan(plan_id, acknowledge=False):
    if not acknowledge:
        raise ValueError('Explicit --i-understand-delete required')
    if not re.fullmatch(r'[a-f0-9]{16}', plan_id):
        raise ValueError('Invalid plan id')
    plan_file = state_dir() / ('plan-' + plan_id + '.json')
    if not plan_file.is_file():
        raise ValueError('Plan not found')
    saved = json.loads(plan_file.read_text(encoding='utf8'))
    root = Path(saved['root']).resolve(strict=True)
    cfg = config(root)
    if saved['project_id'] != cfg['project_id']:
        raise ValueError('Project identity changed')

    protected = {name.casefold() for name in cfg['protected_names']}
    candidates = saved['candidates']
    seen = set()
    for candidate in candidates:
        try:
            _inspect_candidate(root, candidate['relative_path'], cfg, protected, candidate)
        except (OSError, RuntimeError) as exc:
            raise ValueError('Unable to inspect target') from exc
        key = (candidate['device'], candidate['inode'])
        if key in seen:
            raise ValueError('Duplicate target')
        seen.add(key)
    if candidates and os.environ.get('DSG_POC_ALLOW_DELETE') != 'YES':
        raise ValueError('Set DSG_POC_ALLOW_DELETE=YES to enable deletion. This is deliberately disabled by default.')

    deleted = []
    for candidate in candidates:
        rel = candidate['relative_path']
        if len(candidates) > 1:
            _inspect_candidate(root, rel, cfg, protected, candidate)
        shutil.rmtree(root / rel)
        deleted.append(rel)
    audit('gc_apply', root, {'plan_id': plan_id, 'deleted': deleted})
    plan_file.rename(state_dir() / ('applied-' + plan_id + '.json'))
    return {'deleted': deleted, 'plan_id': plan_id}


def docker_inventory():
 if not shutil.which('docker'):return {'available':False,'message':'docker executable not found'}
 out={}
 for key,args in {'system_df':['docker','system','df','--format','json'],'containers':['docker','ps','-a','--format','{{json .}}'],'images':['docker','image','ls','--format','{{json .}}']}.items():
  try:
   r=subprocess.run(args,capture_output=True,text=True,timeout=8,check=False)
   out[key]={'exit':r.returncode,'stdout':r.stdout[:50000],'stderr':r.stderr[:1000]}
  except (OSError,subprocess.TimeoutExpired) as e:out[key]={'error':str(e)}
 return {'available':True,'read_only':True,'data':out}
