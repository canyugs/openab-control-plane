from pathlib import Path
import json,subprocess,os,tempfile,shutil,hashlib
root=Path('/Users/can/Documents/zeabur/ocp-review-commit-integrity');wr=root/'.agentflow/features/review-commit-integrity/artifacts/A-004-linux-runtime'
runroot=Path(Path('/tmp/ocp-linux-runtime-path').read_text().strip());output=runroot/'evaluation';scratch=runroot/'scratch'
live=Path(Path('/tmp/ocp-evaluation-live-path').read_text().strip()).resolve()
def inventory(p):
 return {str(f.relative_to(p)):[hashlib.sha256(f.read_bytes()).hexdigest(),f.stat().st_mtime_ns] for f in p.rglob('*') if f.is_file()}
result=json.loads((wr/'run-result.json').read_text())
assert result['exit']==0 and result['summary']['state']=='complete'
before=inventory(output)
stage=Path(tempfile.mkdtemp(prefix='replay-stage-',dir=scratch))
subprocess.run(['cp','-R','-P',str(live/'repo'),str(stage/'repo')],check=True)
oldargv=result['argv'];oldstage=Path(oldargv[oldargv.index('--repo')+1]).parent
argv=[a.replace(str(oldstage),str(stage)) for a in oldargv]
env=dict(os.environ)
for k in ['CLAUDE_CODE_OAUTH_TOKEN','ANTHROPIC_API_KEY','ANTHROPIC_AUTH_TOKEN']:env.pop(k,None)
print('COMPLETE_REPLAY_WITHOUT_AUTH',flush=True)
try:
 r=subprocess.run(argv,env=env,text=True,capture_output=True)
 print(r.stdout+r.stderr,flush=True);print('REPLAY_EXIT',r.returncode,flush=True)
 assert r.returncode==0
finally:shutil.rmtree(stage)
assert inventory(output)==before
weekly=runroot/'weekly';weekly.mkdir()
bundle=(root/'.agentflow/features/review-commit-integrity/artifacts/A-001-model-evaluation/actual-journey/weekly-bundle').resolve()
argv=['docker','run','--rm','--platform','linux/amd64','--network','none','--user',f'{os.getuid()}:{os.getgid()}','--entrypoint','ocp-review-weekly']
for p,ro in [(bundle,True),(output,True),(weekly,False)]:
 argv+=['--mount',f'type=bind,src={p},dst={p}'+(',readonly' if ro else '')]
argv+=['ocp-review-eval:0.1.0-test','--bundle',str(bundle),'--week','2026-W37','--as-of','2026-09-09T08:10:00+08:00','--evaluation-root',str(output),'--output',str(weekly)]
p=subprocess.run(argv,env=env,text=True,capture_output=True)
print(p.stdout+p.stderr,flush=True);print('WEEKLY_EXIT',p.returncode,flush=True)
assert p.returncode==0
assert inventory(output)==before
pv=Path(Path('/tmp/ocp-package-validation-path').read_text().strip())
assert inventory(live/'evaluation-safe-accepted')==json.loads((pv/'replay-before.json').read_text())
source=subprocess.check_output(['git','-C',str(live/'repo'),'status','--porcelain'],text=True);assert not source
assert not list(scratch.iterdir())
e={'replay_exit':r.returncode,'replay_auth_supplied':False,'replay_unchanged_artifacts':len(before),'weekly_exit':p.returncode,'weekly_argv':argv,'weekly_diagnostic':(p.stdout+p.stderr)[:4096],'old_evidence_unchanged':True,'fixture_repo_clean':True,'scratch_empty':True,'evaluation_inventory':before}
(wr/'readback.json').write_text(json.dumps(e,indent=2)+'\n')
for name in ['evaluation','weekly']:
 shutil.copytree(runroot/name,wr/name)
print('RUNTIME_READBACK_PASS',flush=True)
