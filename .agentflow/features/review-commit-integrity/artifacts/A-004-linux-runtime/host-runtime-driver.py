from pathlib import Path
import json,subprocess,os,time,tempfile,hashlib,shutil
root=Path('/Users/can/Documents/zeabur/ocp-review-commit-integrity')
wr=root/'.agentflow/features/review-commit-integrity/artifacts/A-004-linux-runtime'
live=Path(Path('/tmp/ocp-evaluation-live-path').read_text().strip()).resolve()
runroot=Path(tempfile.mkdtemp(prefix='ocp-linux-auth-evaluation-')).resolve()
Path('/tmp/ocp-linux-runtime-path').write_text(str(runroot))
scratch=runroot/'scratch';output=runroot/'evaluation';scratch.mkdir();output.mkdir()
stage=Path(tempfile.mkdtemp(prefix='repo-stage-',dir=scratch))
subprocess.run(['cp','-R','-P',str(live/'repo'),str(stage/'repo')],check=True)
image='ocp-review-eval:0.1.0-test'
argv=['docker','run','--rm','--init','--platform','linux/amd64','--user',f'{os.getuid()}:{os.getgid()}','--group-add','0']
for p,readonly in [(scratch,False),(output,False),(stage,True),(live/'safe/evidence',True),(live/'safe/findings.json',True),(live/'models.json',True),(live/'environment.json',True)]:
 argv+=['--mount',f'type=bind,src={p},dst={p}'+(',readonly' if readonly else '')]
argv+=['--mount','type=bind,src=/var/run/docker.sock,dst=/var/run/docker.sock','--env','CLAUDE_CODE_OAUTH_TOKEN','--env',f'TMPDIR={scratch}','--env','HOME=/tmp/ocp-review-eval-home','--env','LANG=C.UTF-8','--env','LC_ALL=C.UTF-8',image,'run','--repo',str(stage/'repo'),'--revision','b7927775d4db77f360efe4d96a9b82fc61b7b35c','--base','4ed638bb12f3e1fa11be2524a48e18973ad4591f','--findings',str(live/'safe/findings.json'),'--evidence',str(live/'safe/evidence'),'--models',str(live/'models.json'),'--environment',str(live/'environment.json'),'--output',str(output)]
credentials=subprocess.run(['security','find-generic-password','-s','Claude Code-credentials','-w'],capture_output=True,text=True,check=True)
token=json.loads(credentials.stdout)['claudeAiOauth']['accessToken']
env={**os.environ,'CLAUDE_CODE_OAUTH_TOKEN':token}
e={'argv':argv,'output':str(output),'image':image,'source_kind':'synthetic fixed-SHA accepted fixture, not production PR','credential_source':'Keychain -> process environment; no credential file','started_at_utc':time.strftime('%Y-%m-%dT%H:%M:%SZ',time.gmtime())}
(wr/'run-start.json').write_text(json.dumps(e,indent=2)+'\n')
print('FRESH_LINUX_EVALUATION',str(output),flush=True)
start=time.time()
try:
 p=subprocess.run(argv,env=env,text=True,capture_output=True)
 raw=(p.stdout+p.stderr).replace(token,'[REDACTED]')
 # Fail closed before retaining model/provider artifacts if a credential leaked.
 leaked=[str(f.relative_to(output)) for f in output.rglob('*') if f.is_file() and token.encode() in f.read_bytes()]
 if leaked: raise RuntimeError('Credential readback check failed; do not copy artifacts')
 e.update(exit=p.returncode,elapsed_seconds=round(time.time()-start,3),diagnostic=raw[:4096],credential_leak_found=False)
 if (output/'summary.json').exists():
  s=json.loads((output/'summary.json').read_text());e['summary']=s
 print(raw,flush=True)
 print('EVALUATION_EXIT',p.returncode,flush=True)
 print('EVALUATION_STATE',e.get('summary',{}).get('state'),flush=True)
finally:
 shutil.rmtree(stage)
 e['staging_cleaned']=not stage.exists()
 (wr/'run-result.json').write_text(json.dumps(e,indent=2).replace(token,'[REDACTED]')+'\n')
 token='';env.pop('CLAUDE_CODE_OAUTH_TOKEN',None)
