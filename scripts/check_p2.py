#!/usr/bin/env python3
"""Real two-client loopback impairment lab. All processes are externally sandboxed."""
import argparse,json,os,signal,socket,subprocess,time
from pathlib import Path
from p2_results import describe_demo
root=Path(__file__).resolve().parents[1]
p=argparse.ArgumentParser()
p.add_argument('--profile',choices=['baseline','constrained','loss','outage','setup','blackhole'],default='baseline')
p.add_argument('--quality',choices=['tiny','minimal','low','standard'],default='standard')
p.add_argument('--duration',type=int,default=24,help='Test duration, or impairment schedule length in interactive GUI mode; does not close interactive windows')
p.add_argument('--output',type=Path,default=root/'runs/p2-check')
p.add_argument('--adaptive',action='store_true')
p.add_argument('--audio-only',action='store_true')
p.add_argument('--kbps',type=int,default=0,help='Baseline per-direction cap, including IPv4/UDP overhead')
p.add_argument('--gui',action='store_true',help='Interactive demo; leaving early is normal')
p.add_argument('--verify-gui',action='store_true',help='With --gui, require unattended full-duration checks')
p.add_argument('--audio-profile',choices=['standard','efficient','minimum'],default='standard')
p.add_argument('--peer-audio-profile',choices=['standard','efficient','minimum'],help='Optional different audio profile for peer b')
p.add_argument('--source',choices=['tone','silence'],default='tone',help='CLI synthetic audio source')
p.add_argument('--keyframe-seconds',type=int,choices=range(1,6),default=1)
p.add_argument('--binary',type=Path,default=root/'target/debug/rcof')
p.add_argument('--audio-complexity',type=int,choices=range(11),default=10)
p.add_argument('--no-audio-dtx',action='store_true')
args=p.parse_args()
interactive = args.gui and not args.verify_gui
if args.duration<1 or args.duration>600:p.error('duration must be 1..600 seconds')
if args.profile in ['constrained','loss','outage'] and args.duration<18:p.error('this profile needs at least 18 seconds')
if args.kbps<0 or args.kbps>100000:p.error('kbps must be 0..100000')
if args.verify_gui and not args.gui:p.error('--verify-gui requires --gui')
if args.gui and (args.audio_only or args.profile=='blackhole'):p.error('audio-only and blackhole checks are CLI-only')
args.output.mkdir(parents=True,exist_ok=True)
# Reserve distinct ports until configuration is complete.
sockets=[]
for _ in range(5):
 s=socket.socket(socket.AF_INET,socket.SOCK_DGRAM);s.bind(('127.0.0.1',0));sockets.append(s)
ports=[s.getsockname()[1] for s in sockets]
a,b,ra,rb,server_port=ports
for s in sockets:s.close()
phases=[{'at_s':0,'kbps':args.kbps}]
if args.profile=='constrained':phases += [{'at_s':5,'kbps':128,'delay_ms':30,'jitter_ms':10},{'at_s':args.duration-6}]
if args.profile=='loss':phases += [{'at_s':5,'kbps':384,'delay_ms':40,'jitter_ms':15,'loss_percent':3},{'at_s':args.duration-6}]
if args.profile=='outage':phases += [{'at_s':7,'outage':True},{'at_s':10}]
if args.profile=='setup':phases=[{'at_s':0,'kbps':256,'delay_ms':60,'jitter_ms':10,'loss_percent':1}]
if args.profile=='blackhole':phases=[{'at_s':0,'outage':True}]
config={'a':f'127.0.0.1:{a}','b':f'127.0.0.1:{b}','relay_a':f'127.0.0.1:{ra}','relay_b':f'127.0.0.1:{rb}','duration_s':0 if interactive else args.duration+8,'seed':42,'phases':phases,'queue_bytes':32768,'queue_ms':150}
(args.output/'link-config.json').write_text(json.dumps(config,indent=2))
binary=str(args.binary.resolve());prefix=['/usr/bin/sandbox-exec','-f',str(root/'scripts/loopback-only.sb')]
processes=[];logs=[]
def start(name,command):
 f=(args.output/(name+'.log')).open('w');logs.append(f)
 child=subprocess.Popen(prefix+[binary]+command,stdin=subprocess.PIPE,stdout=f,stderr=subprocess.STDOUT,text=True,start_new_session=True);processes.append(child);return child
try:
 server=start('server',['server','--listen',f'127.0.0.1:{server_port}'])
 relay=start('link',['link','--config',str(args.output/'link-config.json'),'--report',str(args.output/'link.json')])
 for _ in range(100):
  if 'LINK_READY' in (args.output/'link.log').read_text():break
  assert relay.poll() is None,'relay failed'
  time.sleep(.05)
 else:raise AssertionError('relay did not start')
 clients=[]
 for name,port,remote in [('a',a,rb),('b',b,ra)]:
  (args.output/(name+'.json')).unlink(missing_ok=True)
  command=['gui' if args.gui else 'call','--id',name,'--server',f'127.0.0.1:{server_port}','--media-port',str(port),'--peer-relay',str(remote),'--quality',args.quality,'--duration',str(0 if interactive else args.duration),'--stats-file',str(args.output/(name+'.json'))]
  if args.no_audio_dtx:command+=['--no-audio-dtx']
  if args.audio_complexity != 10:command+=['--audio-complexity',str(args.audio_complexity)]
  command+=['--audio-profile',args.peer_audio_profile if name=='b' and args.peer_audio_profile else args.audio_profile,'--keyframe-seconds',str(args.keyframe_seconds)]
  if args.gui:command+=['--auto-join','--exit-after',str(0 if interactive else args.duration+6),'--window-x',str(60 if name=='a' else 420),'--screenshot',str(args.output/(name+'.png')),'--screenshot-after','18']
  else:
   command+=['--source',args.source,'--video','off' if args.audio_only else 'pattern','--connect-timeout','8']
   if args.adaptive:command+=['--adaptive']
  clients.append(start(name,command))
 resources=[]
 next_stats=0.0
 deadline=None if interactive else time.monotonic()+args.duration+25
 if interactive:print('Interactive demo: no call or window timeout. Close both windows or press Ctrl+C to stop. The impairment schedule runs once; its final phase remains active.',flush=True)
 while any(c.poll() is None for c in clients):
  assert deadline is None or time.monotonic()<deadline,'clients did not stop'
  assert relay.poll() is None,'relay stopped unexpectedly; inspect link.log'
  assert server.poll() is None,'server stopped unexpectedly; inspect server.log'
  if interactive:
   for name,c in zip(['a','b'],clients):
    assert c.poll() in (None,0),f'{name}: process crashed with code {c.returncode}; inspect {name}.log'
  if not args.gui and time.monotonic()>=next_stats:
   next_stats=time.monotonic()+1
   rows=[line.split(None,4) for line in subprocess.run(['ps','-axo','pid=,ppid=,%cpu=,rss=,comm='],capture_output=True,text=True).stdout.splitlines()]
   owned={c.pid for c in processes}
   for _ in range(3):owned.update(int(r[0]) for r in rows if int(r[1]) in owned)
   resources.append([r for r in rows if int(r[0]) in owned])
   for c in clients:
    if c.poll() is None:
     try:c.stdin.write('stats\n');c.stdin.flush()
     except BrokenPipeError:pass
  time.sleep(.2)
 (args.output/'resources.json').write_text(json.dumps(resources))
 # Save relay evidence before evaluating either interactive or strict results.
 if relay.poll() is None:relay.send_signal(signal.SIGINT)
 assert relay.wait(timeout=5)==0,'relay failed; inspect link.log'
 if args.gui and not args.verify_gui:
  results=[]
  for name,c in zip(['a','b'],clients):
   path=args.output/(name+'.json')
   results.append((name,c.returncode,json.loads(path.read_text()) if path.exists() else None))
  success,message=describe_demo(results)
  print(message,flush=True)
  print('Logs and measurements:',args.output,flush=True)
  raise SystemExit(0 if success else 1)
 for name,c in zip(['a','b'],clients):
  d=json.loads((args.output/(name+'.json')).read_text())
  if args.profile=='blackhole':
   assert c.returncode!=0 and d['rx_frames']==0,d
  else:
   assert c.returncode==0,(name,d['outcome'])
   assert d['rx_audio_samples']>args.duration*24000,d
   assert d['decode_errors']==0 and d['device_errors']==0,d
   assert (d['video_rx_frames']==0 if args.audio_only else d['video_rx_frames']>0),d
  print(name,{k:d[k] for k in ['rx_frames','video_rx_frames','video_tx_kbps','transport_tx_kbps','sequence_gaps','outcome']},flush=True)
 link=json.loads((args.output/'link.json').read_text())
 for direction in ['a_to_b','b_to_a']:
  c=link[direction]
  assert c['peak_queue_bytes']<=config['queue_bytes'],c
  if args.profile=='blackhole':assert c['delivered_packets']==0 and c['outage_drops']>0,c
  else:assert c['audio_delivered']>0 and (c['video_delivered']==0 if args.audio_only else c['video_delivered']>0),c
  assert c['unexpected_sources']==0,c
  for before,after in zip(link['snapshots'],link['snapshots'][1:]):
   if before['phase']==after['phase']:
    cap=config['phases'][after['phase']].get('kbps',0)
    if cap:
     sent=after[direction]['delivered_ip_bytes']-before[direction]['delivered_ip_bytes']
     # Propagation jitter can move a few packets across a one-second boundary.
     assert sent*8<=cap*1000*(after['elapsed_s']-before['elapsed_s'])+24000,(direction,sent,cap)
  if args.profile=='loss':assert c['loss_drops']>0,c
  if args.profile=='outage':assert c['outage_drops']>0,c
  print(direction,c,flush=True)
 if args.profile=='outage':
  middle=[s for s in link['snapshots'] if 8.1<s['elapsed_s']<9.9]
  before=[s for s in link['snapshots'] if 7.1<s['elapsed_s']<8.1]
  assert middle and before,'missing blackout samples'
  for direction in ['a_to_b','b_to_a']:
   assert middle[-1][direction]['delivered_packets']==before[-1][direction]['delivered_packets'],'packets bypassed blackout'
  for name in ['a','b']:
   samples=[json.loads(line.removeprefix('STATS ')) for line in (args.output/(name+'.log')).read_text().splitlines() if line.startswith(('STATS {','{"id"'))]
   early=[s for s in samples if 6<=s['elapsed_seconds']<=10]
   last=samples[-1]
   assert early and last['rx_audio_samples']>early[-1]['rx_audio_samples']+192000,'audio did not recover'
   assert last['video_rx_frames']>early[-1]['video_rx_frames']+10,'video did not recover'
 if args.profile=='constrained' and args.adaptive:
  assert all(json.loads((args.output/(name+'.json')).read_text())['video_quality_changes']>0 for name in ['a','b']),'adaptation did not run'
 print('PASS:',args.profile,args.quality,flush=True)
except Exception:
 # Child processes write the useful failure reason, not necessarily the parent.
 for path in sorted(args.output.glob('*.log')):
  print(f'\n--- {path.name} (last 6000 characters) ---\n{path.read_text(errors="replace")[-6000:]}',flush=True)
 raise
except KeyboardInterrupt:
 print('Demo interrupted by Ctrl+C. Saving call/link results and stopping owned processes.',flush=True)
finally:
 # Let all calls and the relay save their reports before terminating GUI loops.
 remaining=[c for c in reversed(processes) if c.poll() is None]
 for c in remaining:c.send_signal(signal.SIGINT)
 cleanup_deadline=time.monotonic()+5
 for c in remaining:
  try:c.wait(timeout=max(.01,cleanup_deadline-time.monotonic()))
  except subprocess.TimeoutExpired:
   os.killpg(c.pid,signal.SIGTERM)
   try:c.wait(timeout=1)
   except subprocess.TimeoutExpired:os.killpg(c.pid,signal.SIGKILL);c.wait()
 for f in logs:f.close()
