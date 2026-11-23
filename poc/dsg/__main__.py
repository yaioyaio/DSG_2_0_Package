import argparse,json,sys,os,subprocess,shlex,shutil
from pathlib import Path
from .core import config,scan,plan,apply_plan,deny_command,docker_inventory,DEFAULT, audit

def output(obj):print(json.dumps(obj,indent=2,ensure_ascii=False))
def main():
 p=argparse.ArgumentParser(prog='dsg-poc',description='DSG 2.0 safety-first executable POC')
 sub=p.add_subparsers(dest='cmd',required=True)
 for x in ('doctor','scan','gc-plan','docker-scan'):sub.add_parser(x).add_argument('--root',default='.')
 init=sub.add_parser('init');init.add_argument('--root',default='.')
 check=sub.add_parser('check');check.add_argument('command',nargs=argparse.REMAINDER)
 gc=sub.add_parser('gc-apply');gc.add_argument('plan_id');gc.add_argument('--i-understand-delete',action='store_true')
 hook=sub.add_parser('hook');hook.add_argument('--agent',choices=['claude','codex','gemini'],required=True)
 ex=sub.add_parser('exec');ex.add_argument('--root',default='.');ex.add_argument('command',nargs=argparse.REMAINDER)
 args=p.parse_args()
 try:
  if args.cmd=='init':
   root=Path(args.root).resolve();file=root/'.dsg.json'
   if file.exists():raise ValueError('.dsg.json exists: refusing overwrite')
   d={k:DEFAULT[k] for k in ('version','project_id','project_budget_gb','host_min_free_gb','gc_min_age_hours','scan_dirs')};d['project_id']=root.name
   file.write_text(json.dumps(d,indent=2),encoding='utf8');output({'created':str(file)})
  elif args.cmd=='doctor':
   root=Path(args.root).resolve();output({'root':str(root),'docker_cli_available':bool(shutil.which('docker')),'python':sys.version.split()[0],'config':config(root),'mode':'observe-only by default','warning':'POC is not system-wide enforcement'})
  elif args.cmd=='scan':output(scan(args.root))
  elif args.cmd=='gc-plan':output(plan(args.root))
  elif args.cmd=='gc-apply':output(apply_plan(args.plan_id,args.i_understand_delete))
  elif args.cmd=='docker-scan':output(docker_inventory())
  elif args.cmd=='check':output(deny_command(' '.join(args.command).removeprefix('-- ')))
  elif args.cmd=='exec':
   cmd=args.command[1:] if args.command[:1]==['--'] else args.command
   if not cmd:raise ValueError('Pass command after --')
   decision=deny_command(' '.join(shlex.quote(x) for x in cmd))
   if decision['decision']=='deny':output(decision);return 3
   root=Path(args.root).resolve();s=scan(root);cfg=config(root)
   if s['disk_free_bytes']<cfg['host_min_free_gb']*1024**3:output({'decision':'deny','reason':'host_min_free_gb','disk_free_bytes':s['disk_free_bytes']});return 3
   if s['total_bytes']>cfg['project_budget_gb']*1024**3:output({'decision':'deny','reason':'project_budget_gb'});return 3
   audit('exec',root,{'argv':cmd,'decision':decision})
   return subprocess.run(cmd,cwd=root,shell=False,check=False).returncode
  elif args.cmd=='hook':
   payload=json.load(sys.stdin);t=payload.get('tool_input') or {}
   cmd=t.get('command') or t.get('cmd') or ''
   if not isinstance(cmd,str):cmd=''
   d=deny_command(cmd)
   if d['decision']=='deny':
    if args.agent=='gemini':output({'decision':'deny','reason':'DSG: '+d['reason']})
    else:output({'hookSpecificOutput':{'hookEventName':'PreToolUse','permissionDecision':'deny','permissionDecisionReason':'DSG: '+d['reason']}})
   else:output({})
  return 0
 except Exception as e:
  print('DSG ERROR: '+str(e),file=sys.stderr);return 2
if __name__=='__main__':sys.exit(main())
